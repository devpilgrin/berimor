//! `berimor memory consolidate` (prompt-next-wave.md задача 3): слияние
//! семантических дублей фактов, накопленных со временем — то, что
//! `dedup`/`resolve` на ЗАПИСИ не ловят (например, факт был записан до
//! включения эмбеддингов, §20.23, или похожая формулировка появилась
//! позже отдельным извлечением). Периодическая ручная уборка, не
//! автоматика на каждом прогоне — консолидация меняет id-адресуемые
//! записи (survivor поглощает дубли), делать это на каждом `berimor
//! run` было бы неожиданным побочным эффектом обычного прогона.

use crate::config::Config;
use berimor_memory::semantic::{self, FactId, StoredFact};
use berimor_storage::{EventLog, SemanticStore, SqliteEventLog};
use berimor_types::event::{Event, EventKind, ProcessInstanceId};

/// Синтетический инстанс для событий консолидации — тот же приём, что
/// `"trust-list"`/`"host-sessions"`: консолидация не принадлежит ни
/// одному прогону процесса.
pub const CONSOLIDATION_INSTANCE_ID: &str = "memory-consolidation";

/// Порог близости для слияния — то же калиброванное значение, что
/// `berimor_memory::embeddings::BGE_M3_MERGE_THRESHOLD` (эта крейта не
/// зависит от фичи `embeddings` напрямую — см. `crate::run::facts_embed_fn`,
/// которым получен реальный эмбеддер; константа продублирована здесь как
/// значение, не импортирована, чтобы модуль компилировался и без фичи —
/// без неё команда всё равно откажет раньше, на отсутствии эмбеддера).
const CONSOLIDATION_THRESHOLD: f32 = 0.75;

/// `berimor memory consolidate`. Требует `[memory] embeddings = true` И
/// сборки с `--features embeddings` — без эмбеддера сравнивать факты
/// семантически нечем (точное совпадение уже ловится на записи,
/// `dedup`/`fact_hash`, консолидировать точные дубли не нужно). Тонкая
/// обёртка над [`consolidate_with_embedder`] — сборка реального
/// эмбеддера вынесена сюда, чтобы саму логику слияния можно было
/// протестировать на in-memory хранилище с фейковым замыканием, без
/// фичи `embeddings` и без реальной модели (тот же приём, что
/// `serve::bind`/`serve::run`).
pub fn consolidate(config: &Config) -> Result<(), String> {
    let Some(embed) = crate::run::facts_embed_fn(config.memory.embeddings) else {
        return Err(
            "консолидация требует [memory] embeddings = true И сборки с --features embeddings — без эмбеддера факты нечем сравнивать семантически"
                .to_string(),
        );
    };
    let storage = SqliteEventLog::open(&config.storage_path).map_err(|err| err.to_string())?;
    consolidate_with_embedder(&storage, embed.as_ref())
}

fn consolidate_with_embedder(
    storage: &SqliteEventLog,
    embed: &dyn Fn(&str) -> Result<Vec<f32>, String>,
) -> Result<(), String> {
    let records = storage.all_facts().map_err(|err| err.to_string())?;
    if records.is_empty() {
        println!("[berimor] память: фактов нет — консолидировать нечего");
        return Ok(());
    }

    let facts: Vec<StoredFact> = records
        .iter()
        .map(|r| {
            StoredFact::rehydrate(
                FactId(r.id.clone()),
                r.subject.clone(),
                r.predicate.clone(),
                r.object.clone(),
                r.confidence,
                r.source.clone(),
                r.trusted_channel,
            )
        })
        .collect();

    let mut embeddings = std::collections::HashMap::new();
    for record in &records {
        let text = format!("{} {} {}", record.subject, record.predicate, record.object);
        match embed(&text) {
            Ok(v) => {
                embeddings.insert(record.id.clone(), v);
            }
            Err(err) => eprintln!(
                "[berimor] память: эмбеддинг факта {} не удался — пропущен из консолидации ({err})",
                record.id
            ),
        }
    }

    let groups = semantic::find_consolidation_groups(&facts, &embeddings, CONSOLIDATION_THRESHOLD);
    if groups.is_empty() {
        println!(
            "[berimor] память: близких дублей не найдено (порог {CONSOLIDATION_THRESHOLD}, фактов: {})",
            records.len()
        );
        return Ok(());
    }

    let by_id: std::collections::HashMap<&str, &berimor_storage::FactRecord> =
        records.iter().map(|r| (r.id.as_str(), r)).collect();
    let mut total_merged = 0usize;
    for group in &groups {
        let Some(survivor_record) = by_id.get(group.survivor.0.as_str()) else {
            continue;
        };
        let mut survivor = (*survivor_record).clone();
        let mut merged_descriptions = Vec::new();
        for (dup_id, score) in &group.merged {
            if let Some(dup_record) = by_id.get(dup_id.0.as_str()) {
                survivor.confidence =
                    semantic::merge_confidence(survivor.confidence, dup_record.confidence);
            }
            merged_descriptions.push(format!("{} (схожесть {score:.3})", dup_id.0));
        }
        // Survivor обновляется (уверенность могла вырасти) ДО удаления
        // поглощённых — если удаление откажет, survivor уже усилен, а не
        // оставлен в промежуточном состоянии наполовину.
        storage
            .upsert_fact(&survivor, None)
            .map_err(|err| format!("не удалось обновить survivor {}: {err}", survivor.id))?;
        for (dup_id, _) in &group.merged {
            storage
                .delete_fact(&dup_id.0)
                .map_err(|err| format!("не удалось удалить {}: {err}", dup_id.0))?;
        }
        let detail = format!(
            "survivor {} поглотил [{}]",
            survivor.id,
            merged_descriptions.join(", ")
        );
        // Не молчаливое удаление (prompt-next-wave.md задача 3): событие
        // в тот же журнал, читается `berimor trace` наравне с процессами.
        storage
            .append(Event::new(
                ProcessInstanceId(CONSOLIDATION_INSTANCE_ID.to_string()),
                0,
                EventKind::FactsConsolidated {
                    detail: detail.clone(),
                },
                serde_json::Value::Null,
            ))
            .map_err(|err| format!("не удалось журналировать консолидацию: {err}"))?;
        println!("[berimor] память: {detail}");
        total_merged += group.merged.len();
    }
    println!(
        "[berimor] память: консолидация завершена — {} кластер(ов), {total_merged} факт(ов) слито",
        groups.len()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use berimor_storage::FactRecord;

    fn fact(id: &str, subject: &str, predicate: &str, object: &str, confidence: f32) -> FactRecord {
        FactRecord {
            id: id.into(),
            subject: subject.into(),
            predicate: predicate.into(),
            object: object.into(),
            confidence,
            source: "test".into(),
            trusted_channel: true,
        }
    }

    /// Фейковый эмбеддер: возвращает заранее заданный вектор по тексту —
    /// детерминированно, без реальной модели (та же дисциплина, что
    /// `serve.rs`'s тесты — `consolidate_with_embedder` не знает и не
    /// должен знать, откуда взялся вектор).
    fn fixed_embedder(
        map: std::collections::HashMap<String, Vec<f32>>,
    ) -> impl Fn(&str) -> Result<Vec<f32>, String> {
        move |text: &str| {
            map.get(text)
                .cloned()
                .ok_or_else(|| format!("нет фикстуры для текста «{text}»"))
        }
    }

    #[test]
    fn consolidate_merges_near_duplicate_facts_and_journals_the_merge() {
        let storage = SqliteEventLog::open_in_memory().unwrap();
        storage
            .upsert_fact(&fact("f-1", "клиент c-1", "живёт_в", "Москва", 0.6), None)
            .unwrap();
        storage
            .upsert_fact(
                &fact("f-2", "клиент c-1", "проживает в", "г. Москва", 0.7),
                None,
            )
            .unwrap();
        let embed = fixed_embedder(
            [
                ("клиент c-1 живёт_в Москва".to_string(), vec![1.0, 0.0, 0.0]),
                (
                    "клиент c-1 проживает в г. Москва".to_string(),
                    vec![1.0, 0.0, 0.0],
                ),
            ]
            .into_iter()
            .collect(),
        );

        consolidate_with_embedder(&storage, &embed).unwrap();

        let facts = storage.all_facts().unwrap();
        assert_eq!(facts.len(), 1, "дубль обязан быть удалён");
        assert_eq!(facts[0].id, "f-1", "выживает первый по порядку факт");
        // merge_confidence — максимум: 0.7 > 0.6.
        assert_eq!(facts[0].confidence, 0.7);

        let events = storage
            .replay(&ProcessInstanceId(CONSOLIDATION_INSTANCE_ID.to_string()))
            .unwrap();
        assert_eq!(events.len(), 1, "слияние обязано быть журналировано");
        assert!(matches!(
            &events[0].kind,
            EventKind::FactsConsolidated { detail } if detail.contains("f-1") && detail.contains("f-2")
        ));
    }

    #[test]
    fn consolidate_leaves_unrelated_facts_untouched() {
        let storage = SqliteEventLog::open_in_memory().unwrap();
        storage
            .upsert_fact(&fact("f-1", "клиент c-1", "живёт_в", "Москва", 0.6), None)
            .unwrap();
        storage
            .upsert_fact(&fact("f-2", "клиент c-2", "живёт_в", "Тверь", 0.6), None)
            .unwrap();
        let embed = fixed_embedder(
            [
                ("клиент c-1 живёт_в Москва".to_string(), vec![1.0, 0.0]),
                ("клиент c-2 живёт_в Тверь".to_string(), vec![0.0, 1.0]),
            ]
            .into_iter()
            .collect(),
        );

        consolidate_with_embedder(&storage, &embed).unwrap();

        let facts = storage.all_facts().unwrap();
        assert_eq!(facts.len(), 2, "несвязанные факты не сливаются");
        let events = storage
            .replay(&ProcessInstanceId(CONSOLIDATION_INSTANCE_ID.to_string()))
            .unwrap();
        assert!(events.is_empty(), "слияний не было — журналировать нечего");
    }

    #[test]
    fn consolidate_of_empty_storage_is_a_no_op_not_an_error() {
        let storage = SqliteEventLog::open_in_memory().unwrap();
        let embed = fixed_embedder(std::collections::HashMap::new());

        assert!(consolidate_with_embedder(&storage, &embed).is_ok());
    }

    /// Ошибка эмбеддера для ОДНОГО факта не должна ронять всю
    /// консолидацию — тот факт просто не участвует в кластеризации (та
    /// же деградация, что `facts_layer` на чтении).
    #[test]
    fn consolidate_skips_fact_whose_embedding_fails_without_erroring_the_whole_run() {
        let storage = SqliteEventLog::open_in_memory().unwrap();
        storage
            .upsert_fact(&fact("f-1", "клиент c-1", "живёт_в", "Москва", 0.6), None)
            .unwrap();
        storage
            .upsert_fact(&fact("f-2", "клиент c-2", "живёт_в", "Тверь", 0.6), None)
            .unwrap();
        // Фикстура нарочно не содержит текст f-2 — embed для него вернёт Err.
        let embed = fixed_embedder(
            [("клиент c-1 живёт_в Москва".to_string(), vec![1.0, 0.0])]
                .into_iter()
                .collect(),
        );

        let result = consolidate_with_embedder(&storage, &embed);

        assert!(result.is_ok(), "{result:?}");
        assert_eq!(storage.all_facts().unwrap().len(), 2);
    }

    /// Реальный контракт `SemanticStore::delete_fact` уже проверен в
    /// `berimor-storage`; здесь — что оркестрация действительно его
    /// вызывает для КАЖДОГО поглощённого факта кластера из трёх и более.
    #[test]
    fn consolidate_merges_a_cluster_of_three_into_one_survivor() {
        let storage = SqliteEventLog::open_in_memory().unwrap();
        storage
            .upsert_fact(&fact("f-1", "a", "b", "c1", 0.5), None)
            .unwrap();
        storage
            .upsert_fact(&fact("f-2", "a", "b", "c2", 0.6), None)
            .unwrap();
        storage
            .upsert_fact(&fact("f-3", "a", "b", "c3", 0.9), None)
            .unwrap();
        let embed = fixed_embedder(
            [
                ("a b c1".to_string(), vec![1.0, 0.0]),
                ("a b c2".to_string(), vec![1.0, 0.0]),
                ("a b c3".to_string(), vec![1.0, 0.0]),
            ]
            .into_iter()
            .collect(),
        );

        consolidate_with_embedder(&storage, &embed).unwrap();

        let facts = storage.all_facts().unwrap();
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].id, "f-1");
        assert_eq!(facts[0].confidence, 0.9, "максимум из трёх слитых");
    }
}
