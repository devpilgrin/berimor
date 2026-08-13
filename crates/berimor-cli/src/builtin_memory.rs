//! Инструменты `memory.search` / `memory.save` (спека
//! `docs/rnd/builtin-tools-waves-spec.md`, C8): доступ модели к
//! семантическому слою памяти (MEM3/MEM4) через инструменты.
//!
//! Модуль даёт диспетчер-обёртку [`MemoryToolDispatch`] (прецедент
//! [`crate::agent_dispatch::AgentRunDispatch`],
//! [`crate::builtin_human::HumanAskDispatch`]): `memory.search` и
//! `memory.save` исполняет сам над той же SQLite-БД, что и журнал
//! (`berimor_storage::SqliteEventLog`, ADR-0021 «одно хранилище»),
//! остальные инструменты пробрасывает в `inner`.
//!
//! Дедупликация/конфликты — НЕ своя логика, а существующий semantic API
//! `berimor_memory::semantic::resolve` (точный хэш → конфликт → близкое
//! совпадение, memory-model.md §2) над срезом `SemanticStore::all_facts`,
//! как в записном пути `berimor run`. Близость — [`NoSimilarity`]:
//! провайдера эмбеддингов в контексте инструмента нет, дедуп работает по
//! точному хэшу (тот же режим, что `[memory] embeddings = false`).
//!
//! Отображение свободного текста инструмента в тройку факта:
//! `predicate` — маркер «заметка», `object` — content, `subject` — topic,
//! а при его отсутствии — сам content (тогда subject == object и ложный
//! конфликт невозможен: одинаковый subject означает одинаковый object,
//! что ловится точным хэшем раньше конфликта). Topic выступает именованным
//! слотом: иное содержимое под тем же topic — конфликт (I2, «не молчаливая
//! перезапись»), инструмент отвечает `{status: "conflict"}` и НЕ пишет.
//!
//! Маскировщик — пустой реестр (`Masker::new()`): реестр секретов живёт
//! в запуске `berimor run`, инструменту он недоступен; маскировка
//! содержимого, порождённого моделью, — отдельная задача (та же
//! оговорка, что у `EntityGraphStore::upsert_node`, находка 4 ревью S5).
//!
//! mutates: **false** для обоих (декларирует родитель в
//! `builtin_policies`): хранилище — внутренняя память агента, не
//! пользовательские данные (тот же принцип, что `todo.write` и
//! chat_history в .berimor/); запись дополнительно закрыта флагом
//! конфига `[memory] tool_writes` — доверенная граница декларируется
//! конфигом, не гейтом.

// Проводка (mod + цепочка диспетчеров в run.rs) — клей родителя; до неё
// публичные типы модуля задействованы только в тестах.
#![allow(dead_code)]

use berimor_executors::tool_only::{DispatchError, ToolDispatch};
use berimor_memory::semantic::{
    self, FactId, NoSimilarity, Resolution, StoredFact, DEFAULT_SIMILARITY_THRESHOLD,
};
use berimor_storage::{FactRecord, SemanticStore, SqliteEventLog};
use serde_json::{json, Map, Value};
use std::path::PathBuf;

/// Маркер-предикат фактов, записанных инструментом (см. doc модуля).
const TOOL_PREDICATE: &str = "заметка";
/// limit по умолчанию и его потолок (защита ресурса, как LIST_CAP в
/// files.list): модель не должна вычитывать всю память одним вызовом.
const DEFAULT_SEARCH_LIMIT: usize = 10;
const MAX_SEARCH_LIMIT: usize = 100;

/// Диспетчер-обёртка: `memory.search`/`memory.save` исполняет сам,
/// остальные инструменты делегирует внутренней цепочке. Конструируется
/// родителем в run.rs из конфига (`storage_path`, `[memory] tool_writes`).
pub struct MemoryToolDispatch<'a> {
    /// Путь к SQLite-БД журнала (config.storage_path) — факты живут в
    /// той же БД (ADR-0021). Открывается на каждый вызов: диспетчер
    /// переживает отдельные вызовы, соединение — нет.
    pub storage_path: PathBuf,
    /// `[memory] tool_writes` из конфига: `false` — `memory.save`
    /// отвечает говорящей ошибкой, не записывая ничего.
    pub allow_writes: bool,
    /// Внутренняя цепочка для всех прочих инструментов.
    pub inner: &'a dyn ToolDispatch,
}

impl MemoryToolDispatch<'_> {
    fn open_storage(&self, tool: &str) -> Result<SqliteEventLog, DispatchError> {
        SqliteEventLog::open(&self.storage_path)
            .map_err(|e| crate::builtin_dispatch::err_str(tool, format!("хранилище памяти: {e}")))
    }

    /// Срез существующих фактов в форме semantic API: хэш пересобирается
    /// из полей (`StoredFact::rehydrate`) — тот же приём, что в записном
    /// пути `berimor run`.
    fn load_facts(&self, tool: &str) -> Result<(Vec<FactRecord>, Vec<StoredFact>), DispatchError> {
        let storage = self.open_storage(tool)?;
        let records = storage
            .all_facts()
            .map_err(|e| crate::builtin_dispatch::err_str(tool, format!("чтение фактов: {e}")))?;
        let stored = records
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
        Ok((records, stored))
    }

    fn search(&self, args: &Value) -> Result<Value, DispatchError> {
        let tool = "memory.search";
        let query = args
            .get("query")
            .and_then(Value::as_str)
            .ok_or_else(|| crate::builtin_dispatch::err_str(tool, "аргумент query обязателен"))?;
        let limit = match args.get("limit") {
            None => DEFAULT_SEARCH_LIMIT,
            Some(value) => value
                .as_u64()
                .filter(|n| *n >= 1)
                .map(|n| (n as usize).min(MAX_SEARCH_LIMIT))
                .ok_or_else(|| {
                    crate::builtin_dispatch::err_str(
                        tool,
                        "аргумент limit должен быть положительным целым",
                    )
                })?,
        };

        // Пустой/бестокенный запрос — пустой ответ, не ошибка (спека C8).
        let tokens = tokenize(query);
        if tokens.is_empty() {
            return Ok(json!({"facts": []}));
        }

        let (records, _) = self.load_facts(tool)?;
        // Лексическое ранжирование: доля различных токенов запроса,
        // встретившихся в тексте факта (subject+predicate+object).
        // Эмбеддинг-поиск (SemanticStore::hybrid_search) требует векторов,
        // которых у фактов без провайдера эмбеддингов нет — детерминированная
        // базовая линия работает всегда (тот же принцип, что подстрочный
        // session.search, C9).
        let mut scored: Vec<(usize, &FactRecord)> = records
            .iter()
            .filter_map(|record| {
                let text = format!("{} {} {}", record.subject, record.predicate, record.object)
                    .to_lowercase();
                let hits = tokens.iter().filter(|t| text.contains(t.as_str())).count();
                (hits > 0).then_some((hits, record))
            })
            .collect();
        // Стабильный порядок: оценка по убыванию, затем id (детерминизм
        // ответа при равных оценках).
        scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.id.cmp(&b.1.id)));
        scored.truncate(limit);

        let facts: Vec<Value> = scored
            .into_iter()
            .map(|(_, record)| fact_json(record))
            .collect();
        Ok(json!({"facts": facts}))
    }

    fn save(&self, args: &Value) -> Result<Value, DispatchError> {
        let tool = "memory.save";
        if !self.allow_writes {
            return Err(crate::builtin_dispatch::err_str(
                tool,
                "запись в память через инструмент отключена конфигом ([memory] tool_writes = false)",
            ));
        }
        let content = args
            .get("content")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                crate::builtin_dispatch::err_str(tool, "аргумент content обязателен и непуст")
            })?;
        let topic = match args.get("topic") {
            None => None,
            Some(value) => {
                let topic = value.as_str().ok_or_else(|| {
                    crate::builtin_dispatch::err_str(tool, "аргумент topic должен быть строкой")
                })?;
                let topic = topic.trim();
                (!topic.is_empty()).then(|| topic.to_string())
            }
        };

        // Тройка факта из свободного текста (отображение — в doc модуля).
        let proposal = berimor_mediation::contracts::FactProposal {
            subject: topic.clone().unwrap_or_else(|| content.to_string()),
            predicate: TOOL_PREDICATE.to_string(),
            object: content.to_string(),
            confidence: 1.0,
            source: "tool:memory.save".to_string(),
        };

        let (_records, existing) = self.load_facts(tool)?;
        // Дедуп/конфликт — существующий semantic API (resolve): точный
        // хэш → конфликт → близкое совпадение. NoSimilarity: эмбеддингов
        // в контексте инструмента нет (режим embeddings = false).
        let resolution = semantic::resolve(
            &proposal,
            &existing,
            &NoSimilarity,
            DEFAULT_SIMILARITY_THRESHOLD,
        )
        .map_err(|e| crate::builtin_dispatch::err_str(tool, format!("источник близости: {e}")))?;

        match resolution {
            Resolution::Duplicate { existing } => {
                Ok(json!({"status": "duplicate", "id": existing.0}))
            }
            // I2 «не молчаливая перезапись»: конфликт — ответом, запись
            // НЕ выполняется; разрешение — за человеком/следующей волной.
            Resolution::Conflict(conflict) => Ok(json!({
                "status": "conflict",
                "existing_id": conflict.existing.0,
                "existing_object": conflict.existing_object,
                "candidate_object": conflict.candidate_object,
            })),
            // Недостижимо с NoSimilarity (оценка всегда 0.0 < порога), но
            // ветка исчерпывающая: близкое совпадение — не новая запись.
            Resolution::Merge { existing, .. } => {
                Ok(json!({"status": "duplicate", "id": existing.0}))
            }
            Resolution::New => {
                // Пустой реестр секретов — оговорка в doc модуля (S5).
                let masker = berimor_secrets::Masker::new();
                // Id — от маскированных полей, детерминирован (как в
                // записном пути run): повторное сохранение того же факта
                // упирается в Duplicate по хэшу, а не плодит записи.
                let id = FactId(format!(
                    "f-{}",
                    semantic::fact_hash(
                        &masker.mask_text(&proposal.subject),
                        &masker.mask_text(&proposal.predicate),
                        &masker.mask_text(&proposal.object),
                    )
                    .to_hex()
                ));
                // Канал — недоверенный: content порождён моделью (та же
                // оценка, что у фактов из FactProposalBatch в run).
                let fact = StoredFact::new(id, &proposal, false, &masker);
                let record = FactRecord {
                    id: fact.id.0.clone(),
                    subject: fact.subject.clone(),
                    predicate: fact.predicate.clone(),
                    object: fact.object.clone(),
                    confidence: fact.confidence,
                    source: fact.source.clone(),
                    trusted_channel: fact.trusted_channel,
                };
                let storage = self.open_storage(tool)?;
                storage.upsert_fact(&record, None).map_err(|e| {
                    crate::builtin_dispatch::err_str(tool, format!("запись факта: {e}"))
                })?;
                Ok(json!({"status": "saved", "id": record.id}))
            }
        }
    }
}

impl ToolDispatch for MemoryToolDispatch<'_> {
    fn call(&self, tool: &str, args: &Value) -> Result<Value, DispatchError> {
        match tool {
            "memory.search" => self.search(args),
            "memory.save" => self.save(args),
            _ => self.inner.call(tool, args),
        }
    }
}

/// Различные токены запроса: нижний регистр, разбиение по
/// не-буквенно-цифровым символам (кириллица — alphanumeric, работает).
fn tokenize(query: &str) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut tokens = Vec::new();
    for token in query
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|s| !s.is_empty())
    {
        if seen.insert(token.to_string()) {
            tokens.push(token.to_string());
        }
    }
    tokens
}

/// Проекция факта в ответ инструмента: content — object; topic — subject,
/// когда он отличается от object (у фактов инструмента без topic
/// subject == object по построению, см. doc модуля; у фактов записного
/// пути run subject — естественная «тема» тройки).
fn fact_json(record: &FactRecord) -> Value {
    let mut fact = Map::new();
    fact.insert("id".to_string(), json!(record.id));
    fact.insert("content".to_string(), json!(record.object));
    if record.subject != record.object {
        fact.insert("topic".to_string(), json!(record.subject));
    }
    Value::Object(fact)
}

#[cfg(test)]
mod tests {
    use super::*;
    use berimor_executors::tool_only::StaticToolDispatch;

    /// Temp-файл БД по конвенции спеки (berimor-<mod>-test-<tag>-<pid>):
    /// tag различает тесты модуля (гонка temp-каталогов), pid — повторные
    /// запуски. open_in_memory() не подходит: диспетчер открывает БД по
    /// пути на каждый вызов, соединение между вызовами не живёт.
    fn temp_db(tag: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "berimor-mem-test-{tag}-{}.sqlite",
            std::process::id()
        ));
        cleanup(&path); // идемпотентность повторного запуска
        path
    }

    fn cleanup(path: &PathBuf) {
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(path.with_extension("sqlite-wal"));
        let _ = std::fs::remove_file(path.with_extension("sqlite-shm"));
    }

    fn inner_stub() -> StaticToolDispatch {
        StaticToolDispatch::new(vec![("other.tool".to_string(), json!({"ok": true}), false)])
    }

    fn dispatch<'a>(
        path: &std::path::Path,
        allow_writes: bool,
        inner: &'a StaticToolDispatch,
    ) -> MemoryToolDispatch<'a> {
        MemoryToolDispatch {
            storage_path: path.to_path_buf(),
            allow_writes,
            inner,
        }
    }

    #[test]
    fn save_выключен_говорящая_ошибка() {
        let path = temp_db("disabled");
        let inner = inner_stub();
        let d = dispatch(&path, false, &inner);
        let err = d
            .call("memory.save", &json!({"content": "факт"}))
            .expect_err("должна быть ошибка");
        assert_eq!(err.tool, "memory.save");
        assert!(
            err.reason.contains("отключена конфигом"),
            "текст: {}",
            err.reason
        );
        // Ничего не записалось: БД даже не создана инструментом.
        let d_on = dispatch(&path, true, &inner);
        let out = d_on
            .call("memory.search", &json!({"query": "факт"}))
            .expect("поиск");
        assert_eq!(out, json!({"facts": []}));
        cleanup(&path);
    }

    #[test]
    fn save_search_круг() {
        let path = temp_db("round");
        let inner = inner_stub();
        let d = dispatch(&path, true, &inner);

        let saved = d
            .call(
                "memory.save",
                &json!({"content": "любимый цвет клиента — синий", "topic": "клиент"}),
            )
            .expect("сохранение");
        assert_eq!(saved["status"], json!("saved"));
        let id = saved["id"].as_str().expect("id").to_string();
        assert!(id.starts_with("f-"), "id: {id}");

        let out = d
            .call("memory.search", &json!({"query": "цвет клиента"}))
            .expect("поиск");
        let facts = out["facts"].as_array().expect("массив");
        assert_eq!(facts.len(), 1, "ответ: {out}");
        assert_eq!(facts[0]["id"], json!(id));
        assert_eq!(facts[0]["content"], json!("любимый цвет клиента — синий"));
        assert_eq!(facts[0]["topic"], json!("клиент"));
        cleanup(&path);
    }

    #[test]
    fn save_без_topic_ищется_без_topic_в_ответе() {
        let path = temp_db("notopic");
        let inner = inner_stub();
        let d = dispatch(&path, true, &inner);
        d.call("memory.save", &json!({"content": "релиз в пятницу"}))
            .expect("сохранение");
        let out = d
            .call("memory.search", &json!({"query": "релиз"}))
            .expect("поиск");
        let facts = out["facts"].as_array().expect("массив");
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0]["content"], json!("релиз в пятницу"));
        assert!(facts[0].get("topic").is_none(), "ответ: {out}");
        cleanup(&path);
    }

    #[test]
    fn дубликат_не_пишется_дважды() {
        let path = temp_db("dup");
        let inner = inner_stub();
        let d = dispatch(&path, true, &inner);
        let first = d
            .call("memory.save", &json!({"content": "один и тот же факт"}))
            .expect("первое сохранение");
        assert_eq!(first["status"], json!("saved"));
        let second = d
            .call("memory.save", &json!({"content": "один и тот же факт"}))
            .expect("второе сохранение");
        assert_eq!(second["status"], json!("duplicate"));
        assert_eq!(second["id"], first["id"]);

        let out = d
            .call("memory.search", &json!({"query": "факт"}))
            .expect("поиск");
        assert_eq!(out["facts"].as_array().expect("массив").len(), 1);
        cleanup(&path);
    }

    #[test]
    fn конфликт_того_же_topic_без_перезаписи() {
        let path = temp_db("conflict");
        let inner = inner_stub();
        let d = dispatch(&path, true, &inner);
        d.call(
            "memory.save",
            &json!({"content": "пароль ротируем еженедельно", "topic": "политика"}),
        )
        .expect("первое сохранение");
        let conflict = d
            .call(
                "memory.save",
                &json!({"content": "пароль ротируем ежемесячно", "topic": "политика"}),
            )
            .expect("конфликт — ответ, не ошибка");
        assert_eq!(conflict["status"], json!("conflict"));
        assert_eq!(
            conflict["existing_object"],
            json!("пароль ротируем еженедельно")
        );

        // Старое значение не перезаписано (I2).
        let out = d
            .call("memory.search", &json!({"query": "пароль"}))
            .expect("поиск");
        let facts = out["facts"].as_array().expect("массив");
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0]["content"], json!("пароль ротируем еженедельно"));
        cleanup(&path);
    }

    #[test]
    fn пустая_память_пустой_ответ_не_ошибка() {
        let path = temp_db("empty");
        let inner = inner_stub();
        let d = dispatch(&path, true, &inner);
        let out = d
            .call("memory.search", &json!({"query": "что угодно"}))
            .expect("поиск");
        assert_eq!(out, json!({"facts": []}));
        cleanup(&path);
    }

    #[test]
    fn limit_ограничивает_выдачу() {
        let path = temp_db("limit");
        let inner = inner_stub();
        let d = dispatch(&path, true, &inner);
        for i in 0..5 {
            d.call(
                "memory.save",
                &json!({"content": format!("заметка номер {i} про проект")}),
            )
            .expect("сохранение");
        }
        let out = d
            .call("memory.search", &json!({"query": "проект", "limit": 2}))
            .expect("поиск");
        assert_eq!(out["facts"].as_array().expect("массив").len(), 2);
        cleanup(&path);
    }

    #[test]
    fn не_memory_инструмент_пробрасывается_в_inner() {
        let path = temp_db("passthrough");
        let inner = inner_stub();
        let d = dispatch(&path, true, &inner);
        let out = d
            .call("other.tool", &json!({"x": 1}))
            .expect("ответ заглушки");
        assert_eq!(out, json!({"ok": true}));
        cleanup(&path);
    }

    #[test]
    fn save_без_content_ошибка() {
        let path = temp_db("nocontent");
        let inner = inner_stub();
        let d = dispatch(&path, true, &inner);
        let err = d
            .call("memory.save", &json!({}))
            .expect_err("должна быть ошибка");
        assert!(err.reason.contains("content"), "текст: {}", err.reason);
        cleanup(&path);
    }

    #[test]
    fn search_без_query_ошибка() {
        let path = temp_db("noquery");
        let inner = inner_stub();
        let d = dispatch(&path, true, &inner);
        let err = d
            .call("memory.search", &json!({}))
            .expect_err("должна быть ошибка");
        assert!(err.reason.contains("query"), "текст: {}", err.reason);
        cleanup(&path);
    }
}
