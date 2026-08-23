//! Qdrant-адаптер SemanticStore (волна E, 0.42.0): семантический поиск
//! фактов по HNSW-индексу Qdrant вместо полного скана SQLite — для
//! больших объёмов фактов (ROADMAP §21, волна E). Протокол — чистый
//! HTTP/JSON (reqwest blocking), без gRPC-клиента: зависимостей не
//! прибавляет, поведение прозрачно.
//!
//! Маппинг: коллекция `berimor_facts` (cosine), точка = факт; id точки —
//! u64-хэш строкового `FactRecord.id` (Qdrant принимает только u64/uuid),
//! сам id — в payload.fact_id. Точки без эмбеддинга хранятся payload-only
//! (Qdrant допускает); поиск их не находит — то же соглашение, что у
//! SQLite-версии (`cosine_similarity` → None).
//!
//! hybrid_search: кандидаты = векторный поиск (limit*5) ∪ текстовые
//! совпадения из scroll (subject/predicate/object содержат запрос),
//! combined = VECTOR_WEIGHT·score + TEXT_WEIGHT·text — те же веса, что у
//! SQLite-реализации, результаты сопоставимы.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use serde_json::{json, Value};

use crate::{FactRecord, HybridHit, SemanticStore, StorageError};

/// Qdrant поверх HTTP.
pub struct QdrantStore {
    base_url: String,
    collection: String,
    api_key: Option<String>,
    client: reqwest::blocking::Client,
}

fn point_id(fact_id: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    fact_id.hash(&mut hasher);
    hasher.finish()
}

fn payload_of(fact: &FactRecord) -> Value {
    json!({
        "fact_id": fact.id,
        "subject": fact.subject,
        "predicate": fact.predicate,
        "object": fact.object,
        "confidence": fact.confidence,
        "source": fact.source,
        "trusted_channel": fact.trusted_channel,
    })
}

fn fact_of(payload: &Value) -> FactRecord {
    FactRecord {
        id: payload["fact_id"].as_str().unwrap_or_default().to_string(),
        subject: payload["subject"].as_str().unwrap_or_default().to_string(),
        predicate: payload["predicate"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
        object: payload["object"].as_str().unwrap_or_default().to_string(),
        confidence: payload["confidence"].as_f64().unwrap_or(0.0) as f32,
        source: payload["source"].as_str().unwrap_or_default().to_string(),
        trusted_channel: payload["trusted_channel"].as_bool().unwrap_or(false),
    }
}

impl QdrantStore {
    pub fn new(base_url: &str, collection: &str, api_key: Option<String>) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            collection: collection.to_string(),
            api_key,
            client: reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .expect("reqwest client"),
        }
    }

    fn request(&self, method: reqwest::Method, path: &str) -> reqwest::blocking::RequestBuilder {
        let url = format!("{}{}", self.base_url, path);
        let builder = self.client.request(method, url);
        match &self.api_key {
            Some(key) => builder.header("api-key", key),
            None => builder,
        }
    }

    fn call(
        &self,
        method: reqwest::Method,
        path: &str,
        body: &Value,
    ) -> Result<Value, StorageError> {
        let response = self
            .request(method, path)
            .json(body)
            .send()
            .map_err(|err| StorageError::Unavailable(format!("qdrant {path}: {err}")))?;
        let status = response.status();
        let text = response
            .text()
            .map_err(|err| StorageError::Unavailable(format!("qdrant {path}: чтение: {err}")))?;
        if !status.is_success() {
            return Err(StorageError::Unavailable(format!(
                "qdrant {path}: {status}: {}",
                &text[..text.len().min(300)]
            )));
        }
        serde_json::from_str(&text)
            .map_err(|err| StorageError::Unavailable(format!("qdrant {path}: не JSON: {err}")))
    }

    /// Коллекция создаётся лениво по размерности первого эмбеддинга;
    /// «уже существует» — не ошибка (идемпотентно между процессами).
    fn ensure_collection(&self, size: usize) -> Result<(), StorageError> {
        let path = format!("/collections/{}", self.collection);
        if self
            .request(reqwest::Method::GET, &path)
            .send()
            .map(|r| r.status().is_success())
            .unwrap_or(false)
        {
            return Ok(());
        }
        self.call(
            reqwest::Method::PUT,
            &path,
            &json!({"vectors": {"size": size, "distance": "Cosine"}}),
        )?;
        Ok(())
    }
}

impl SemanticStore for QdrantStore {
    fn upsert_fact(
        &self,
        fact: &FactRecord,
        embedding: Option<&[f32]>,
    ) -> Result<(), StorageError> {
        if let Some(vector) = embedding {
            self.ensure_collection(vector.len())?;
        } else {
            self.ensure_collection(384)?; // payload-only: размерность формальна
        }
        let mut point = json!({
            "id": point_id(&fact.id),
            "payload": payload_of(fact),
        });
        if let Some(vector) = embedding {
            point["vector"] = json!(vector);
        }
        self.call(
            reqwest::Method::PUT,
            &format!("/collections/{}/points?wait=true", self.collection),
            &json!({"points": [point]}),
        )?;
        Ok(())
    }

    fn all_facts(&self) -> Result<Vec<FactRecord>, StorageError> {
        let mut facts = Vec::new();
        let mut offset = Value::Null;
        loop {
            let mut body = json!({"limit": 256, "with_payload": true, "with_vector": false});
            if !offset.is_null() {
                body["offset"] = offset;
            }
            let response = self.call(
                reqwest::Method::POST,
                &format!("/collections/{}/points/scroll", self.collection),
                &body,
            )?;
            let points = response["result"]["points"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            for point in &points {
                facts.push(fact_of(&point["payload"]));
            }
            match &response["result"]["next_page_offset"] {
                Value::Null => break,
                next => offset = next.clone(),
            }
            if points.is_empty() {
                break;
            }
        }
        Ok(facts)
    }

    fn cosine_similarity(
        &self,
        fact_id: &str,
        query_embedding: &[f32],
    ) -> Result<Option<f32>, StorageError> {
        let response = self.call(
            reqwest::Method::POST,
            &format!("/collections/{}/points/search", self.collection),
            &json!({
                "vector": query_embedding,
                "limit": 1,
                "with_payload": false,
                "filter": {"must": [{"key": "fact_id", "match": {"value": fact_id}}]},
            }),
        )?;
        Ok(response["result"]
            .as_array()
            .and_then(|hits| hits.first())
            .and_then(|hit| hit["score"].as_f64())
            .map(|score| score as f32))
    }

    fn hybrid_search(
        &self,
        query_text: &str,
        query_embedding: &[f32],
        limit: usize,
    ) -> Result<Vec<HybridHit>, StorageError> {
        use std::collections::HashMap;
        // (факт, векторная часть, текст-совпадение) — поля HybridHit
        // заполняются честно, не из суммы задним числом.
        let mut combined: HashMap<String, (FactRecord, f32, bool)> = HashMap::new();
        // Векторные кандидаты (если коллекции ещё нет — пусто, не ошибка).
        if !query_embedding.is_empty() {
            if let Ok(response) = self.call(
                reqwest::Method::POST,
                &format!("/collections/{}/points/search", self.collection),
                &json!({
                    "vector": query_embedding,
                    "limit": limit.saturating_mul(5).max(10),
                    "with_payload": true,
                }),
            ) {
                for hit in response["result"].as_array().cloned().unwrap_or_default() {
                    let fact = fact_of(&hit["payload"]);
                    let score = hit["score"].as_f64().unwrap_or(0.0) as f32;
                    combined.insert(fact.id.clone(), (fact, score, false));
                }
            }
        }
        // Текстовые кандидаты (полнотекст по payload).
        if !query_text.is_empty() {
            let needle = query_text.to_lowercase();
            for fact in self.all_facts()? {
                let haystack = format!(
                    "{} {} {}",
                    fact.subject.to_lowercase(),
                    fact.predicate.to_lowercase(),
                    fact.object.to_lowercase()
                );
                if haystack.contains(&needle) {
                    combined
                        .entry(fact.id.clone())
                        .and_modify(|(_, _, text)| *text = true)
                        .or_insert((fact, 0.0, true));
                }
            }
        }
        let mut hits: Vec<HybridHit> = combined
            .into_values()
            .map(|(fact, vector_score, text_matched)| HybridHit {
                fact_id: fact.id,
                vector_score,
                text_matched,
                combined_score: vector_score * crate::VECTOR_WEIGHT
                    + if text_matched {
                        crate::TEXT_WEIGHT
                    } else {
                        0.0
                    },
            })
            .collect();
        hits.sort_by(|a, b| {
            b.combined_score
                .partial_cmp(&a.combined_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        hits.truncate(limit);
        Ok(hits)
    }

    fn delete_fact(&self, id: &str) -> Result<(), StorageError> {
        self.call(
            reqwest::Method::POST,
            &format!("/collections/{}/points/delete?wait=true", self.collection),
            &json!({"points": [point_id(id)]}),
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn point_id_is_deterministic_u64() {
        assert_eq!(point_id("fact-1"), point_id("fact-1"));
        assert_ne!(point_id("fact-1"), point_id("fact-2"));
    }

    /// Живой прогон против реального Qdrant (QDRANT_URL, обычно
    /// http://127.0.0.1:6333): полный цикл upsert → all → cosine →
    /// hybrid → delete. #[ignore] в CI, запуск по требованию.
    #[test]
    #[ignore]
    fn live_qdrant_roundtrip() {
        let url =
            std::env::var("QDRANT_URL").unwrap_or_else(|_| "http://127.0.0.1:6333".to_string());
        let store = QdrantStore::new(&url, "berimor_facts_test", None);
        let fact = FactRecord {
            id: "live-1".into(),
            subject: "пользователь".into(),
            predicate: "предпочитает".into(),
            object: "тёмную тему".into(),
            confidence: 0.9,
            source: "test".into(),
            trusted_channel: true,
        };
        let embedding: Vec<f32> = (0..384).map(|i| (i as f32 * 0.001).sin()).collect();
        store.upsert_fact(&fact, Some(&embedding)).unwrap();
        let all = store.all_facts().unwrap();
        assert!(all.iter().any(|f| f.id == "live-1"), "{all:?}");
        let sim = store.cosine_similarity("live-1", &embedding).unwrap();
        assert!(sim.unwrap_or(0.0) > 0.99, "{sim:?}");
        let hits = store.hybrid_search("тёмную тему", &embedding, 5).unwrap();
        assert!(hits.iter().any(|h| h.fact_id == "live-1"), "{hits:?}");
        store.delete_fact("live-1").unwrap();
        assert!(!store.all_facts().unwrap().iter().any(|f| f.id == "live-1"));
    }

    #[test]
    fn payload_roundtrip_preserves_fact() {
        let fact = FactRecord {
            id: "f-1".into(),
            subject: "пользователь".into(),
            predicate: "предпочитает".into(),
            object: "тёмную тему".into(),
            confidence: 0.9,
            source: "chat".into(),
            trusted_channel: true,
        };
        let restored = fact_of(&payload_of(&fact));
        assert_eq!(restored.id, fact.id);
        assert_eq!(restored.subject, fact.subject);
        assert_eq!(restored.predicate, fact.predicate);
        assert_eq!(restored.object, fact.object);
        assert!((restored.confidence - fact.confidence).abs() < 1e-6);
        assert_eq!(restored.source, fact.source);
        assert!(restored.trusted_channel);
    }
}
