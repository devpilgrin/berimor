//! Кэш ответов модели по ТОЧНОМУ хэшу (волна E, 0.42.0): ключ —
//! провайдер + system_context + prompt + контракт + схема. Детерминизм
//! сохранён: одинаковый запрос → байт-в-байт тот же ответ (модель не
//! вызывается вовсе, usage в журнал не пишется — вызова не было).
//! Похожесть эмбеддингов НЕ используется (недетерминизм ответа —
//! осознанно, см. ROADMAP §21).
//!
//! Хранилище — отдельный SQLite `<storage>.cache.db` (кэш одноразовый:
//! удаление файла = полная инвалидация). Включается
//! `[agent] response_cache = true` (по умолчанию выключен).
//!
//! Порядок обёрток: кэш СНАРУЖИ метра — попадание не доходит до
//! провайдера и не журналирует usage (его нет).

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use berimor_types::executor::ModelProvider;
use berimor_types::model::{CompletionRequest, CompletionResponse, ModelError};

/// Ключ кэша: весь смысловой вход вызова (модель включена — ответы
/// разных моделей не перемешиваются).
fn cache_key(provider: &str, request: &CompletionRequest) -> String {
    let mut hasher = DefaultHasher::new();
    provider.hash(&mut hasher);
    request.system_context.hash(&mut hasher);
    request.prompt.hash(&mut hasher);
    request.contract_name.hash(&mut hasher);
    request.expects_structured_output.hash(&mut hasher);
    request
        .json_schema
        .as_ref()
        .map(|s| s.to_string())
        .hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// SQLite-хранилище кэша (одна таблица, без миграций — файл одноразовый).
pub struct CacheStore {
    conn: Mutex<rusqlite::Connection>,
}

impl CacheStore {
    pub fn open(path: &Path) -> Result<Self, String> {
        let conn = rusqlite::Connection::open(path)
            .map_err(|err| format!("кэш {}: {err}", path.display()))?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS llm_cache (
                key TEXT PRIMARY KEY,
                raw_text TEXT NOT NULL,
                provider TEXT NOT NULL,
                model_id TEXT NOT NULL,
                created_ms INTEGER NOT NULL
            )",
        )
        .map_err(|err| format!("кэш: схема: {err}"))?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn get(&self, key: &str) -> Option<(String, String, String)> {
        self.conn
            .lock()
            .expect("cache lock")
            .query_row(
                "SELECT raw_text, provider, model_id FROM llm_cache WHERE key = ?1",
                [key],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .ok()
    }

    fn put(&self, key: &str, raw_text: &str, provider: &str, model_id: &str) {
        let created_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let _ = self.conn.lock().expect("cache lock").execute(
            "INSERT OR REPLACE INTO llm_cache (key, raw_text, provider, model_id, created_ms)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![key, raw_text, provider, model_id, created_ms],
        );
    }
}

/// Путь к файлу кэша рядом с журналом: `<storage>.cache.db`.
pub fn cache_path(storage_path: &Path) -> PathBuf {
    PathBuf::from(format!("{}.cache.db", storage_path.display()))
}

/// Обёртка: точный кэш перед реальным провайдером. Ключ включает имя
/// провайдера — ответы разных моделей не перемешиваются.
pub struct CachingProvider {
    inner: Arc<dyn ModelProvider + Send + Sync>,
    store: Arc<CacheStore>,
    name: String,
}

impl CachingProvider {
    pub fn wrap(
        inner: Arc<dyn ModelProvider + Send + Sync>,
        store: Arc<CacheStore>,
        name: String,
    ) -> Arc<dyn ModelProvider + Send + Sync> {
        Arc::new(Self { inner, store, name })
    }
}

impl ModelProvider for CachingProvider {
    fn complete(&self, request: CompletionRequest) -> Result<CompletionResponse, ModelError> {
        let key = cache_key(&self.name, &request);
        if let Some((raw_text, provider, model_id)) = self.store.get(&key) {
            return Ok(CompletionResponse {
                raw_text,
                model: berimor_types::model::ModelIdentity {
                    provider,
                    model_id,
                    tier: berimor_types::model::ModelTier::Weak,
                },
                usage: Some(berimor_types::model::TokenUsage {
                    prompt_tokens: 0,
                    completion_tokens: 0,
                }),
            });
        }
        let response = self.inner.complete(request)?;
        self.store.put(
            &key,
            &response.raw_text,
            &response.model.provider,
            &response.model.model_id,
        );
        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use berimor_types::model::{CompletionRequest, ModelIdentity, ModelTier};
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Counting {
        calls: AtomicUsize,
    }

    impl ModelProvider for Counting {
        fn complete(&self, _request: CompletionRequest) -> Result<CompletionResponse, ModelError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(CompletionResponse {
                raw_text: "ответ".into(),
                model: ModelIdentity {
                    provider: "stub".into(),
                    model_id: "m1".into(),
                    tier: ModelTier::Strong,
                },
                usage: None,
            })
        }
    }

    fn request(prompt: &str) -> CompletionRequest {
        CompletionRequest {
            system_context: "sys".into(),
            prompt: prompt.into(),
            contract_name: None,
            expects_structured_output: false,
            json_schema: None,
            step_id: None,
        }
    }

    #[test]
    fn hit_avoids_provider_call_and_returns_same_text() {
        let dir = std::env::temp_dir().join(format!("berimor-cache-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Arc::new(CacheStore::open(&dir.join("c.db")).unwrap());
        let counting = Arc::new(Counting {
            calls: AtomicUsize::new(0),
        });
        let cached = CachingProvider::wrap(counting.clone(), store, "stub".to_string());
        let first = cached.complete(request("привет")).unwrap();
        let second = cached.complete(request("привет")).unwrap();
        assert_eq!(first.raw_text, second.raw_text);
        assert_eq!(counting.calls.load(Ordering::SeqCst), 1);
        // Другой запрос — другой ключ, провайдер вызван снова.
        let _ = cached.complete(request("другое")).unwrap();
        assert_eq!(counting.calls.load(Ordering::SeqCst), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
