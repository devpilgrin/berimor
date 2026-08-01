//! `berimor-storage` — единый встраиваемый движок хранения.
//!
//! Источник: `docs/arch/stack.md` §3, `docs/arch/memory-model.md`, ADR-0021: события,
//! снапшоты, полнотекст (FTS5), векторы (sqlite-vec) и граф сущностей — в
//! одном файле SQLite, а не в четырёх разных хранилищах.
//!
//! ROADMAP: F1 (события/снапшоты — реализовано) · MEM2 (полнотекст) · MEM4 (векторы) · MEM7 (граф).

use berimor_types::event::{Event, EventKind, EventSeq, ProcessInstanceId, Snapshot};
use rusqlite::{params, Connection, OptionalExtension};
use std::sync::{Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

/// Единственный источник истины для журнала событий инстанса.
/// Реализация — SQLite, WAL, один писатель на инстанс (`process-engine.md` §4).
///
/// `seq` и `ts_ms` в `Event`, переданном в [`EventLog::append`], игнорируются:
/// хранилище присваивает их атомарно при записи и возвращает настоящий `seq`.
/// Это исключает гонку двух писателей за номер следующего события —
/// см. [`Event::new`](berimor_types::event::Event::new).
pub trait EventLog {
    fn append(&self, event: Event) -> Result<EventSeq, StorageError>;
    /// Все события инстанса в порядке записи. Неизвестный инстанс — пустой
    /// вектор, не ошибка: отсутствие истории не отличимо от ещё не начатой.
    fn replay(&self, process_instance: &ProcessInstanceId) -> Result<Vec<Event>, StorageError>;
    fn write_snapshot(&self, snapshot: Snapshot) -> Result<(), StorageError>;
    fn latest_snapshot(
        &self,
        process_instance: &ProcessInstanceId,
    ) -> Result<Option<Snapshot>, StorageError>;
}

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("хранилище недоступно: {0}")]
    Unavailable(String),
    #[error("нарушение целостности журнала: {0}")]
    Corrupt(String),
}

impl From<rusqlite::Error> for StorageError {
    fn from(err: rusqlite::Error) -> Self {
        StorageError::Unavailable(err.to_string())
    }
}

impl From<serde_json::Error> for StorageError {
    fn from(err: serde_json::Error) -> Self {
        StorageError::Corrupt(err.to_string())
    }
}

const SCHEMA_SQL: &str = "
CREATE TABLE IF NOT EXISTS events (
    process_instance_id TEXT NOT NULL,
    seq                 INTEGER NOT NULL,
    process_version     INTEGER NOT NULL,
    kind                TEXT NOT NULL,
    payload             TEXT NOT NULL,
    ts_ms               INTEGER NOT NULL,
    PRIMARY KEY (process_instance_id, seq)
);
CREATE TABLE IF NOT EXISTS snapshots (
    process_instance_id TEXT NOT NULL,
    seq                 INTEGER NOT NULL,
    state               TEXT NOT NULL,
    PRIMARY KEY (process_instance_id, seq)
);
-- Эпизодическая память (MEM2, memory-model.md §1): полнотекстовый индекс
-- по журналу событий — «сессия» = process_instance_id. Отдельная
-- (не external-content) FTS5-таблица: несёт свои копии
-- process_instance_id/seq/ts_ms, чтобы результат поиска не требовал
-- обратного джойна в `events` по составному ключу.
CREATE VIRTUAL TABLE IF NOT EXISTS events_fts USING fts5(
    process_instance_id UNINDEXED,
    seq                 UNINDEXED,
    ts_ms               UNINDEXED,
    kind_text,
    payload_text
);
-- Семантическая память (MEM4, memory-model.md §3): персистентность
-- фактов (MEM3, berimor-memory::semantic) + гибридный поиск. `embedding`
-- хранится как JSON-массив в обычном текстовом столбце, не в
-- vec0-виртуальной таблице: скалярная функция `vec_distance_cosine` из
-- sqlite-vec покрывает всё, что нужно этому milestone (парная близость,
-- ранжирование по близости в WHERE/ORDER BY) без обязательства заранее
-- фиксировать размерность эмбеддинга в DDL, которое потребовал бы vec0.
-- ANN-индексация vec0 — оптимизация масштаба, не корректности; отложена
-- до реального сценария, где полный скан таблицы фактов станет узким
-- местом (сейчас такого сценария нет).
CREATE TABLE IF NOT EXISTS facts (
    id               TEXT PRIMARY KEY,
    subject          TEXT NOT NULL,
    predicate        TEXT NOT NULL,
    object           TEXT NOT NULL,
    confidence       REAL NOT NULL,
    source           TEXT NOT NULL,
    trusted_channel  INTEGER NOT NULL,
    embedding        TEXT
);
CREATE VIRTUAL TABLE IF NOT EXISTS facts_fts USING fts5(
    fact_id UNINDEXED,
    subject,
    predicate,
    object
);
-- Граф сущностей (MEM7, memory-model.md §4, ADR-0016): персистентность
-- узлов/рёбер, разрешённых entity resolution `berimor_memory::entity_graph`
-- (эта таблица — сырьё для её чистых функций, работающих над срезами
-- `&[StoredNode]`/`&[StoredEdge]`, не над хранилищем напрямую — тот же
-- принцип разделения слоёв, что MEM3/MEM4: логика резолюции не знает про
-- SQLite, хранилище не знает про типы `berimor-memory`). `properties` —
-- JSON-объект произвольной формы (как `payload` у событий), не
-- раскладывается по столбцам: набор полей зависит от `node_type`/
-- `edge_type` и проверяется схемой типа на уровне `berimor-memory`, не
-- здесь. Профиль/тенант не хранится отдельным столбцом — граф сущностей
-- изолируется правилами `MemoryLayer::Semantic` (`profile.rs`), тем же
-- способом, что `facts` уже сегодня.
CREATE TABLE IF NOT EXISTS nodes (
    id         TEXT PRIMARY KEY,
    node_type  TEXT NOT NULL,
    properties TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS nodes_node_type ON nodes (node_type);
CREATE TABLE IF NOT EXISTS edges (
    id         TEXT PRIMARY KEY,
    edge_type  TEXT NOT NULL,
    source     TEXT NOT NULL,
    target     TEXT NOT NULL,
    properties TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS edges_source ON edges (source);
CREATE INDEX IF NOT EXISTS edges_target ON edges (target);
-- Акторы (A1, stack.md §68): почтовый ящик персистентен — «конверт
-- считается доставленным только после фиксации в журнале SQLite».
-- delivered=0 после сбоя между «отправлено» и «обработано» — то же
-- восстановление после падения, что и у Process Engine (замена ручного
-- requeue после аварийной заморозки, A6: незавершённая обработка просто
-- не помечена доставленной, подбирается заново).
CREATE TABLE IF NOT EXISTS envelopes (
    id         TEXT PRIMARY KEY,
    from_actor TEXT NOT NULL,
    to_actor   TEXT NOT NULL,
    topic      TEXT NOT NULL,
    payload    TEXT NOT NULL,
    delivered  INTEGER NOT NULL DEFAULT 0,
    ts_ms      INTEGER NOT NULL
);
-- Планировщик (A5, stack.md §69): «персистентный min-heap ближайших
-- срабатываний» — B-tree индекс по next_fire_ms играет роль min-heap
-- (peek-min = ORDER BY next_fire_ms LIMIT, pop = UPDATE/DELETE due строк
-- в одной транзакции — тот же приём, что защищает от двойного тика).
CREATE TABLE IF NOT EXISTS schedules (
    id            TEXT PRIMARY KEY,
    next_fire_ms  INTEGER NOT NULL,
    interval_ms   INTEGER,
    payload       TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS schedules_next_fire_ms ON schedules (next_fire_ms);
";

/// Встраиваемая реализация [`EventLog`] на SQLite (ADR-0021).
pub struct SqliteEventLog {
    conn: Mutex<Connection>,
}

impl SqliteEventLog {
    /// Открывает (или создаёт) файл журнала по пути `path`, включает WAL и
    /// применяет схему. WAL — условие «один писатель, читатели не
    /// блокируются» (`process-engine.md` §4).
    pub fn open(path: &std::path::Path) -> Result<Self, StorageError> {
        register_vec_extension();
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        Self::from_connection(conn)
    }

    /// In-memory журнал — для тестов и для Milestone 0 (`docs/ROADMAP.md` §3).
    /// WAL для `:memory:` не имеет смысла (нет файла) — не включается.
    pub fn open_in_memory() -> Result<Self, StorageError> {
        register_vec_extension();
        let conn = Connection::open_in_memory()?;
        Self::from_connection(conn)
    }

    fn from_connection(conn: Connection) -> Result<Self, StorageError> {
        conn.execute_batch(SCHEMA_SQL)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn lock(&self) -> Result<MutexGuard<'_, Connection>, StorageError> {
        self.conn.lock().map_err(|_| {
            StorageError::Corrupt("соединение с журналом повреждено (poisoned lock)".into())
        })
    }
}

impl EventLog for SqliteEventLog {
    fn append(&self, event: Event) -> Result<EventSeq, StorageError> {
        let conn = self.lock()?;
        let kind_json = serde_json::to_string(&event.kind)?;
        let payload_json = serde_json::to_string(&event.payload)?;
        let ts_ms = now_ms();

        // INSERT..SELECT — один атомарный оператор: следующий seq для
        // инстанса вычисляется и записывается без отдельного шага чтения,
        // поэтому гонка между двумя `append` на один инстанс невозможна
        // (см. также правило «один писатель на инстанс», process-engine.md §4).
        conn.execute(
            "INSERT INTO events (process_instance_id, seq, process_version, kind, payload, ts_ms)
             SELECT ?1, COALESCE(MAX(seq), 0) + 1, ?2, ?3, ?4, ?5
             FROM events WHERE process_instance_id = ?1",
            params![
                event.process_instance.0,
                event.process_version,
                kind_json,
                payload_json,
                ts_ms
            ],
        )?;

        let seq: i64 = conn.query_row(
            "SELECT MAX(seq) FROM events WHERE process_instance_id = ?1",
            params![event.process_instance.0],
            |row| row.get(0),
        )?;

        // Индекс эпизодической памяти (MEM2) наполняется в той же операции,
        // что и сам журнал — не отдельным проходом, чтобы поиск никогда не
        // отставал от того, что реально записано (не «почти всегда
        // синхронизировано»).
        conn.execute(
            "INSERT INTO events_fts (process_instance_id, seq, ts_ms, kind_text, payload_text)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                event.process_instance.0,
                seq,
                ts_ms,
                kind_json,
                payload_json
            ],
        )?;

        Ok(EventSeq(seq as u64))
    }

    fn replay(&self, process_instance: &ProcessInstanceId) -> Result<Vec<Event>, StorageError> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT seq, process_version, kind, payload, ts_ms FROM events
             WHERE process_instance_id = ?1 ORDER BY seq ASC",
        )?;
        let rows = stmt.query_map(params![process_instance.0], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, u32>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
            ))
        })?;

        let mut events = Vec::new();
        for row in rows {
            let (seq, process_version, kind_json, payload_json, ts_ms) = row?;
            let kind: EventKind = serde_json::from_str(&kind_json)?;
            let payload: serde_json::Value = serde_json::from_str(&payload_json)?;
            events.push(Event {
                seq: EventSeq(seq as u64),
                process_instance: process_instance.clone(),
                process_version,
                kind,
                payload,
                ts_ms,
            });
        }
        Ok(events)
    }

    fn write_snapshot(&self, snapshot: Snapshot) -> Result<(), StorageError> {
        let conn = self.lock()?;
        let state_json = serde_json::to_string(&snapshot.state)?;
        conn.execute(
            "INSERT INTO snapshots (process_instance_id, seq, state) VALUES (?1, ?2, ?3)
             ON CONFLICT(process_instance_id, seq) DO UPDATE SET state = excluded.state",
            params![
                snapshot.process_instance.0,
                snapshot.seq.0 as i64,
                state_json
            ],
        )?;
        Ok(())
    }

    fn latest_snapshot(
        &self,
        process_instance: &ProcessInstanceId,
    ) -> Result<Option<Snapshot>, StorageError> {
        let conn = self.lock()?;
        let row = conn
            .query_row(
                "SELECT seq, state FROM snapshots WHERE process_instance_id = ?1
                 ORDER BY seq DESC LIMIT 1",
                params![process_instance.0],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?;

        match row {
            Some((seq, state_json)) => {
                let state: serde_json::Value = serde_json::from_str(&state_json)?;
                Ok(Some(Snapshot {
                    process_instance: process_instance.clone(),
                    seq: EventSeq(seq as u64),
                    state,
                }))
            }
            None => Ok(None),
        }
    }
}

/// Одно совпадение полнотекстового поиска по эпизодической памяти (MEM2).
/// `process_instance` — идентификатор сессии, к которой принадлежит событие
/// (`memory-model.md` §1: «сессии, события шагов, решения, отчёты»).
#[derive(Debug, Clone, PartialEq)]
pub struct EpisodeHit {
    pub process_instance: ProcessInstanceId,
    pub seq: EventSeq,
    pub kind: EventKind,
    pub payload: serde_json::Value,
    pub ts_ms: i64,
}

/// Полнотекстовый поиск по журналу событий — «поиск по сессиям»
/// (`memory-model.md` §1, ROADMAP: MEM2). Отдельный трейт, не метод
/// [`EventLog`]: это независимая возможность (чтение для памяти, не
/// событийный источник истины для движка процесса) с единственной
/// реализацией — той же самой, но по другой причине, чем `EventLog`.
pub trait EpisodicSearch {
    /// Ранжированные по релевантности (FTS5 `rank`) совпадения по всем
    /// сессиям, не более `limit`. Пустой или состоящий только из
    /// FTS5-спецсимволов запрос — пустой результат, не ошибка: нет
    /// доказуемого намерения искать что-то конкретное.
    fn search_episodes(&self, query: &str, limit: usize) -> Result<Vec<EpisodeHit>, StorageError>;
}

/// FTS5 `MATCH` — собственный язык запросов (`"фраза"`, `-исключение`,
/// `col:термин`, `*`, `^` и т.д.). Строка из свободного текста пользователя
/// или сессии не обязана быть валидным запросом на этом языке — при прямой
/// передаче нераспознанный синтаксис возвращает ошибку SQL вместо
/// предсказуемого результата поиска. Оставляем только буквы/цифры/`_`
/// (токен-символы `unicode61` по умолчанию) — это то, что реально участвует
/// в сопоставлении, остальное для FTS5 всё равно только разделители.
fn sanitize_query(raw: &str) -> String {
    raw.split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .filter(|term| !term.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

impl EpisodicSearch for SqliteEventLog {
    fn search_episodes(&self, query: &str, limit: usize) -> Result<Vec<EpisodeHit>, StorageError> {
        let sanitized = sanitize_query(query);
        if sanitized.is_empty() {
            return Ok(Vec::new());
        }

        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT process_instance_id, seq, ts_ms, kind_text, payload_text
             FROM events_fts WHERE events_fts MATCH ?1 ORDER BY rank LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![sanitized, limit as i64], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?;

        let mut hits = Vec::new();
        for row in rows {
            let (process_instance_id, seq, ts_ms, kind_json, payload_json) = row?;
            hits.push(EpisodeHit {
                process_instance: ProcessInstanceId(process_instance_id),
                seq: EventSeq(seq as u64),
                kind: serde_json::from_str(&kind_json)?,
                payload: serde_json::from_str(&payload_json)?,
                ts_ms,
            });
        }
        Ok(hits)
    }
}

/// Регистрирует расширение `sqlite-vec` (MEM4) для всех СОЕДИНЕНИЙ,
/// открытых ПОСЛЕ вызова — `sqlite3_auto_extension` в SQLite глобален на
/// процесс и не действует на уже открытые соединения, поэтому вызывается
/// в начале `open`/`open_in_memory`, до `Connection::open*`. SQLite
/// дедуплицирует повторную регистрацию той же точки входа сама, но
/// `Once` — дешевле и явнее, чем полагаться на это молча при каждом
/// вызове `open`.
fn register_vec_extension() {
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| unsafe {
        rusqlite::ffi::sqlite3_auto_extension(Some(std::mem::transmute::<
            *const (),
            unsafe extern "C" fn(
                *mut rusqlite::ffi::sqlite3,
                *mut *mut std::os::raw::c_char,
                *const rusqlite::ffi::sqlite3_api_routines,
            ) -> std::os::raw::c_int,
        >(
            sqlite_vec::sqlite3_vec_init as *const ()
        )));
    });
}

/// Факт семантической памяти (MEM3, `berimor_memory::semantic::StoredFact`)
/// в форме хранилища — те же поля, без типа `FactId`/`FactHash`
/// `berimor-memory` (эта крейта не зависит от `berimor-memory`, только
/// наоборот, ADR-0021: хранилище — общий фундамент, не наоборот).
#[derive(Debug, Clone, PartialEq)]
pub struct FactRecord {
    pub id: String,
    pub subject: String,
    pub predicate: String,
    pub object: String,
    pub confidence: f32,
    pub source: String,
    pub trusted_channel: bool,
}

/// Одно совпадение гибридного поиска (MEM4, `memory-model.md` §3):
/// векторная близость + полнотекстовое совпадение, объединённые
/// фиксированными весами в `combined_score`.
#[derive(Debug, Clone, PartialEq)]
pub struct HybridHit {
    pub fact_id: String,
    pub vector_score: f32,
    pub text_matched: bool,
    pub combined_score: f32,
}

/// Веса гибридного поиска — `memory-model.md` §3 требует «фиксированные
/// веса», но не называет числа. Вектор — основной сигнал (уже
/// нормализован в `[0.0, 1.0]`, при наличии эмбеддинга обычно
/// информативнее одного факта совпадения текста), полнотекст —
/// усиливающий буст поверх него. Стартовые константы кода до
/// офлайн-калибровки (Фаза 9), как `DEFAULT_SIMILARITY_THRESHOLD` (MEM3)
/// и `context_engine::budget_chars` (C3).
pub const VECTOR_WEIGHT: f32 = 0.7;
pub const TEXT_WEIGHT: f32 = 0.3;

/// Персистентность и гибридный поиск семантической памяти (MEM4).
/// Работает с [`FactRecord`] — простыми полями хранилища, не с типами
/// `berimor-memory` (та зависит от этой крейты, не наоборот).
pub trait SemanticStore {
    /// Пишет факт целиком; `embedding: None` не стирает уже сохранённый
    /// эмбеддинг (обновление остальных полей без эмбеддинга — обычный
    /// случай, пока не появился провайдер эмбеддингов), `Some` —
    /// заменяет его полностью.
    fn upsert_fact(&self, fact: &FactRecord, embedding: Option<&[f32]>)
        -> Result<(), StorageError>;
    /// Все факты — сырьё для `berimor_memory::semantic::resolve`, которая
    /// сама решает точное/близкое совпадение и конфликт по срезу,
    /// который эта функция загружает из хранилища.
    fn all_facts(&self) -> Result<Vec<FactRecord>, StorageError>;
    /// Косинусная близость `[0.0, 1.0]` уже сохранённого эмбеддинга факта
    /// к `query_embedding`. `None` — факта нет или у него нет эмбеддинга
    /// (не 0.0 — отсутствие данных не то же самое, что доказанная
    /// непохожесть).
    fn cosine_similarity(
        &self,
        fact_id: &str,
        query_embedding: &[f32],
    ) -> Result<Option<f32>, StorageError>;
    /// Гибридный поиск по всем фактам с известным эмбеддингом: векторная
    /// близость к `query_embedding` + полнотекстовое совпадение
    /// `query_text` по subject/predicate/object, объединённые
    /// [`VECTOR_WEIGHT`]/[`TEXT_WEIGHT`]. Убывающий порядок по
    /// `combined_score`, не более `limit`.
    fn hybrid_search(
        &self,
        query_text: &str,
        query_embedding: &[f32],
        limit: usize,
    ) -> Result<Vec<HybridHit>, StorageError>;
}

fn embedding_to_json(embedding: &[f32]) -> String {
    let values: Vec<String> = embedding.iter().map(|v| v.to_string()).collect();
    format!("[{}]", values.join(","))
}

impl SemanticStore for SqliteEventLog {
    fn upsert_fact(
        &self,
        fact: &FactRecord,
        embedding: Option<&[f32]>,
    ) -> Result<(), StorageError> {
        let conn = self.lock()?;
        let embedding_json = embedding.map(embedding_to_json);
        conn.execute(
            "INSERT INTO facts (id, subject, predicate, object, confidence, source, trusted_channel, embedding)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(id) DO UPDATE SET
                subject = excluded.subject,
                predicate = excluded.predicate,
                object = excluded.object,
                confidence = excluded.confidence,
                source = excluded.source,
                trusted_channel = excluded.trusted_channel,
                embedding = COALESCE(excluded.embedding, facts.embedding)",
            params![
                fact.id,
                fact.subject,
                fact.predicate,
                fact.object,
                fact.confidence,
                fact.source,
                fact.trusted_channel,
                embedding_json
            ],
        )?;

        // facts_fts не поддерживает UPDATE по значению неиндексируемого
        // ключа напрямую — пересоздаём запись целиком, как events_fts (MEM2)
        // делает при каждом append (там — вставка новой, здесь —
        // потенциальное обновление уже существующей).
        conn.execute("DELETE FROM facts_fts WHERE fact_id = ?1", params![fact.id])?;
        conn.execute(
            "INSERT INTO facts_fts (fact_id, subject, predicate, object) VALUES (?1, ?2, ?3, ?4)",
            params![fact.id, fact.subject, fact.predicate, fact.object],
        )?;
        Ok(())
    }

    fn all_facts(&self) -> Result<Vec<FactRecord>, StorageError> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT id, subject, predicate, object, confidence, source, trusted_channel FROM facts",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(FactRecord {
                id: row.get(0)?,
                subject: row.get(1)?,
                predicate: row.get(2)?,
                object: row.get(3)?,
                confidence: row.get::<_, f64>(4)? as f32,
                source: row.get(5)?,
                trusted_channel: row.get(6)?,
            })
        })?;
        let mut facts = Vec::new();
        for row in rows {
            facts.push(row?);
        }
        Ok(facts)
    }

    fn cosine_similarity(
        &self,
        fact_id: &str,
        query_embedding: &[f32],
    ) -> Result<Option<f32>, StorageError> {
        let conn = self.lock()?;
        let distance: Option<f64> = conn
            .query_row(
                "SELECT vec_distance_cosine(embedding, ?2) FROM facts
                 WHERE id = ?1 AND embedding IS NOT NULL",
                params![fact_id, embedding_to_json(query_embedding)],
                |row| row.get(0),
            )
            .optional()?;
        Ok(distance.map(|d| (1.0 - d as f32).clamp(0.0, 1.0)))
    }

    fn hybrid_search(
        &self,
        query_text: &str,
        query_embedding: &[f32],
        limit: usize,
    ) -> Result<Vec<HybridHit>, StorageError> {
        let conn = self.lock()?;

        let sanitized = sanitize_query(query_text);
        let matched_ids: std::collections::HashSet<String> = if sanitized.is_empty() {
            Default::default()
        } else {
            let mut stmt =
                conn.prepare("SELECT fact_id FROM facts_fts WHERE facts_fts MATCH ?1")?;
            let rows = stmt.query_map(params![sanitized], |row| row.get::<_, String>(0))?;
            let mut ids = std::collections::HashSet::new();
            for row in rows {
                ids.insert(row?);
            }
            ids
        };

        let embedding_json = embedding_to_json(query_embedding);
        let mut stmt = conn.prepare(
            "SELECT id, vec_distance_cosine(embedding, ?1) FROM facts WHERE embedding IS NOT NULL",
        )?;
        let rows = stmt.query_map(params![embedding_json], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)?))
        })?;

        let mut hits = Vec::new();
        for row in rows {
            let (fact_id, distance) = row?;
            let vector_score = (1.0 - distance as f32).clamp(0.0, 1.0);
            let text_matched = matched_ids.contains(&fact_id);
            let combined_score =
                VECTOR_WEIGHT * vector_score + TEXT_WEIGHT * if text_matched { 1.0 } else { 0.0 };
            hits.push(HybridHit {
                fact_id,
                vector_score,
                text_matched,
                combined_score,
            });
        }

        hits.sort_by(|a, b| b.combined_score.total_cmp(&a.combined_score));
        hits.truncate(limit);
        Ok(hits)
    }
}

/// Узел графа сущностей (MEM7, `berimor_memory::entity_graph::StoredNode`)
/// в форме хранилища — те же поля, без типа `NodeId` `berimor-memory`
/// (эта крейта не зависит от `berimor-memory`, только наоборот, ADR-0021).
#[derive(Debug, Clone, PartialEq)]
pub struct NodeRecord {
    pub id: String,
    pub node_type: String,
    pub properties: serde_json::Value,
}

/// Ребро графа сущностей (MEM7, `StoredEdge`) в форме хранилища — те же
/// поля, `source`/`target` как простые `id`-строки.
#[derive(Debug, Clone, PartialEq)]
pub struct EdgeRecord {
    pub id: String,
    pub edge_type: String,
    pub source: String,
    pub target: String,
    pub properties: serde_json::Value,
}

/// Персистентность графа сущностей (MEM7). Работает с [`NodeRecord`]/
/// [`EdgeRecord`] — простыми полями хранилища, не с типами
/// `berimor-memory`. Резолюцию/конфликты (кто есть кто, какие рёбра
/// противоречат друг другу) решает `berimor_memory::entity_graph` над
/// срезом, который эти методы загружают из хранилища — тот же принцип
/// разделения, что [`SemanticStore::all_facts`] для MEM3/MEM4.
pub trait EntityGraphStore {
    /// Пишет узел целиком; тот же id — обновление уже существующего, а
    /// не дубликат (`ON CONFLICT DO UPDATE`, как у [`SemanticStore::upsert_fact`]).
    ///
    /// Граница маскировки (S5): этот слой НЕ маскирует `properties` —
    /// хранилище не знает реестра секретов запуска и не должно знать.
    /// Маскировка — обязанность продюсера записи, по образцу
    /// `berimor_memory::semantic::StoredFact::new` (маскировщик на записи,
    /// memory-model.md §5). Продюсеров с секретами в properties сейчас
    /// нет; появление такого продюсера без маскировки на его стороне —
    /// осознанный риск, зафиксированный здесь (находка 4 независимого
    /// ревью S5).
    fn upsert_node(&self, node: &NodeRecord) -> Result<(), StorageError>;
    /// Все узлы — сырьё для `berimor_memory::entity_graph::resolve_node`,
    /// которая сама решает точное/близкое совпадение по срезу.
    fn all_nodes(&self) -> Result<Vec<NodeRecord>, StorageError>;
    /// Пишет ребро целиком; тот же id — обновление, не дубликат.
    fn upsert_edge(&self, edge: &EdgeRecord) -> Result<(), StorageError>;
    /// Все рёбра — сырьё для `detect_edge_conflict`/`resolve_edge`.
    fn all_edges(&self) -> Result<Vec<EdgeRecord>, StorageError>;
}

impl EntityGraphStore for SqliteEventLog {
    fn upsert_node(&self, node: &NodeRecord) -> Result<(), StorageError> {
        let conn = self.lock()?;
        let properties_json = serde_json::to_string(&node.properties)?;
        conn.execute(
            "INSERT INTO nodes (id, node_type, properties)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(id) DO UPDATE SET
                node_type = excluded.node_type,
                properties = excluded.properties",
            params![node.id, node.node_type, properties_json],
        )?;
        Ok(())
    }

    fn all_nodes(&self) -> Result<Vec<NodeRecord>, StorageError> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare("SELECT id, node_type, properties FROM nodes")?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        let mut nodes = Vec::new();
        for row in rows {
            let (id, node_type, properties_text) = row?;
            nodes.push(NodeRecord {
                id,
                node_type,
                properties: serde_json::from_str(&properties_text)?,
            });
        }
        Ok(nodes)
    }

    fn upsert_edge(&self, edge: &EdgeRecord) -> Result<(), StorageError> {
        let conn = self.lock()?;
        let properties_json = serde_json::to_string(&edge.properties)?;
        conn.execute(
            "INSERT INTO edges (id, edge_type, source, target, properties)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(id) DO UPDATE SET
                edge_type = excluded.edge_type,
                source = excluded.source,
                target = excluded.target,
                properties = excluded.properties",
            params![
                edge.id,
                edge.edge_type,
                edge.source,
                edge.target,
                properties_json
            ],
        )?;
        Ok(())
    }

    fn all_edges(&self) -> Result<Vec<EdgeRecord>, StorageError> {
        let conn = self.lock()?;
        let mut stmt =
            conn.prepare("SELECT id, edge_type, source, target, properties FROM edges")?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?;
        let mut edges = Vec::new();
        for row in rows {
            let (id, edge_type, source, target, properties_text) = row?;
            edges.push(EdgeRecord {
                id,
                edge_type,
                source,
                target,
                properties: serde_json::from_str(&properties_text)?,
            });
        }
        Ok(edges)
    }
}

/// Идентификатор конверта.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EnvelopeId(pub String);

/// Конверт — единица межакторной коммуникации (ADR-0009, A1). Подпись
/// (A2, `berimor_actors::signing`/`berimor_actors::bus`) — поверх этой
/// структуры, отдельно от вызова публикации, не поле здесь: подпись
/// проверяется один раз на шине, дальше конверт в системе — уже
/// доверенные данные, таскать подпись вместе с ним незачем. Здесь —
/// только то, что нужно для адресации и доставки.
#[derive(Debug, Clone, PartialEq)]
pub struct Envelope {
    pub id: EnvelopeId,
    pub from: String,
    pub to: String,
    pub topic: String,
    pub payload: serde_json::Value,
}

/// Персистентность почтовых ящиков акторов (A1, `stack.md` §68):
/// «конверт считается доставленным только после фиксации в журнале
/// SQLite» — падение процесса между «отправлено» и «записано» не теряет
/// сообщение, то же свойство отказоустойчивости, что и у Process Engine.
pub trait MailboxLog {
    /// Журналирует конверт как ожидающий доставки — до попытки передать
    /// его в почтовый ящик получателя.
    fn persist_envelope(&self, envelope: &Envelope) -> Result<(), StorageError>;
    /// Помечает конверт доставленным — вызывается ПОСЛЕ того, как
    /// получатель успешно обработал его, не в момент постановки в канал:
    /// аварийная заморозка (A6) посреди обработки оставляет конверт
    /// недоставленным, он подбирается заново при восстановлении —
    /// «активные задачи не теряются, а ставятся обратно в очередь»
    /// (`security-model.md` §4) реализуется тем, что requeue-шага просто
    /// не существует, недоставленное и так остаётся в очереди.
    fn mark_delivered(&self, id: &EnvelopeId) -> Result<(), StorageError>;
    /// Недоставленные конверты для актора в порядке записи —
    /// восстановление почтового ящика после старта/сбоя/разморозки.
    fn undelivered_for(&self, actor: &str) -> Result<Vec<Envelope>, StorageError>;
}

impl MailboxLog for SqliteEventLog {
    fn persist_envelope(&self, envelope: &Envelope) -> Result<(), StorageError> {
        let conn = self.lock()?;
        let payload_json = serde_json::to_string(&envelope.payload)?;
        conn.execute(
            "INSERT INTO envelopes (id, from_actor, to_actor, topic, payload, delivered, ts_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, 0, ?6)",
            params![
                envelope.id.0,
                envelope.from,
                envelope.to,
                envelope.topic,
                payload_json,
                now_ms()
            ],
        )?;
        Ok(())
    }

    fn mark_delivered(&self, id: &EnvelopeId) -> Result<(), StorageError> {
        let conn = self.lock()?;
        conn.execute(
            "UPDATE envelopes SET delivered = 1 WHERE id = ?1",
            params![id.0],
        )?;
        Ok(())
    }

    fn undelivered_for(&self, actor: &str) -> Result<Vec<Envelope>, StorageError> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT id, from_actor, to_actor, topic, payload FROM envelopes
             WHERE to_actor = ?1 AND delivered = 0 ORDER BY ts_ms ASC",
        )?;
        let rows = stmt.query_map(params![actor], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?;
        let mut envelopes = Vec::new();
        for row in rows {
            let (id, from, to, topic, payload_json) = row?;
            envelopes.push(Envelope {
                id: EnvelopeId(id),
                from,
                to,
                topic,
                payload: serde_json::from_str(&payload_json)?,
            });
        }
        Ok(envelopes)
    }
}

/// Идентификатор расписания.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ScheduleId(pub String);

/// Расписание — одно ближайшее время срабатывания (A5). Персистентный
/// min-heap (`stack.md` §69) — B-tree индекс по `next_fire_ms` играет его
/// роль: peek-min — `ORDER BY next_fire_ms LIMIT`, pop — продвижение/
/// удаление сработавших строк в одной транзакции ([`ScheduleStore::tick`]).
#[derive(Debug, Clone, PartialEq)]
pub struct Schedule {
    pub id: ScheduleId,
    pub next_fire_ms: i64,
    /// `None` — одноразовое расписание (после срабатывания удаляется);
    /// `Some(ms)` — повторяющееся, следующее срабатывание — сработавшее
    /// время плюс интервал.
    pub interval_ms: Option<i64>,
    pub payload: serde_json::Value,
}

pub trait ScheduleStore {
    fn upsert_schedule(&self, schedule: &Schedule) -> Result<(), StorageError>;
    fn cancel_schedule(&self, id: &ScheduleId) -> Result<(), StorageError>;
    /// Забирает все расписания, готовые сработать к `now_ms`, и
    /// атомарно (одна транзакция) продвигает их: повторяющиеся — на
    /// `next_fire_ms + interval_ms` (второй тик до наступления нового
    /// времени их уже не увидит — защита от двойного срабатывания),
    /// одноразовые — удаляются. Возвращает расписания в состоянии ДО
    /// продвижения — то, что реально сработало на этом тике.
    fn tick(&self, now_ms: i64) -> Result<Vec<Schedule>, StorageError>;
    /// То же, что [`ScheduleStore::tick`] по выборке, но БЕЗ продвижения
    /// или удаления — peek, не pop (A4, ADR-0015): гейт `human_gate`
    /// проверяет, что было бы due, не потребляя срабатывание, пока
    /// очередь подтверждений не разобрана — расписание остаётся due и
    /// попробует снова на следующем тике.
    fn due(&self, now_ms: i64) -> Result<Vec<Schedule>, StorageError>;
}

impl ScheduleStore for SqliteEventLog {
    fn upsert_schedule(&self, schedule: &Schedule) -> Result<(), StorageError> {
        let conn = self.lock()?;
        let payload_json = serde_json::to_string(&schedule.payload)?;
        conn.execute(
            "INSERT INTO schedules (id, next_fire_ms, interval_ms, payload) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(id) DO UPDATE SET
                next_fire_ms = excluded.next_fire_ms,
                interval_ms = excluded.interval_ms,
                payload = excluded.payload",
            params![
                schedule.id.0,
                schedule.next_fire_ms,
                schedule.interval_ms,
                payload_json
            ],
        )?;
        Ok(())
    }

    fn cancel_schedule(&self, id: &ScheduleId) -> Result<(), StorageError> {
        let conn = self.lock()?;
        conn.execute("DELETE FROM schedules WHERE id = ?1", params![id.0])?;
        Ok(())
    }

    fn tick(&self, now_ms: i64) -> Result<Vec<Schedule>, StorageError> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;

        let due: Vec<(String, i64, Option<i64>, String)> = {
            let mut stmt = tx.prepare(
                "SELECT id, next_fire_ms, interval_ms, payload FROM schedules
                 WHERE next_fire_ms <= ?1 ORDER BY next_fire_ms ASC",
            )?;
            let rows = stmt.query_map(params![now_ms], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })?;
            rows.collect::<Result<Vec<_>, rusqlite::Error>>()?
        };

        let mut fired = Vec::with_capacity(due.len());
        for (id, next_fire_ms, interval_ms, payload_json) in due {
            match interval_ms {
                Some(interval) => {
                    tx.execute(
                        "UPDATE schedules SET next_fire_ms = next_fire_ms + ?2 WHERE id = ?1",
                        params![id, interval],
                    )?;
                }
                None => {
                    tx.execute("DELETE FROM schedules WHERE id = ?1", params![id])?;
                }
            }
            fired.push(Schedule {
                id: ScheduleId(id),
                next_fire_ms,
                interval_ms,
                payload: serde_json::from_str(&payload_json)?,
            });
        }
        tx.commit()?;
        Ok(fired)
    }

    fn due(&self, now_ms: i64) -> Result<Vec<Schedule>, StorageError> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT id, next_fire_ms, interval_ms, payload FROM schedules
             WHERE next_fire_ms <= ?1 ORDER BY next_fire_ms ASC",
        )?;
        let rows = stmt.query_map(params![now_ms], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, Option<i64>>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?;
        let mut schedules = Vec::new();
        for row in rows {
            let (id, next_fire_ms, interval_ms, payload_json) = row?;
            schedules.push(Schedule {
                id: ScheduleId(id),
                next_fire_ms,
                interval_ms,
                payload: serde_json::from_str(&payload_json)?,
            });
        }
        Ok(schedules)
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn pid(s: &str) -> ProcessInstanceId {
        ProcessInstanceId(s.to_string())
    }

    #[test]
    fn append_assigns_sequential_seq_per_instance() {
        let log = SqliteEventLog::open_in_memory().unwrap();
        let instance = pid("inst-1");

        let seq1 = log
            .append(Event::new(
                instance.clone(),
                1,
                EventKind::Snapshot,
                json!({"n": 1}),
            ))
            .unwrap();
        let seq2 = log
            .append(Event::new(
                instance.clone(),
                1,
                EventKind::Snapshot,
                json!({"n": 2}),
            ))
            .unwrap();

        assert_eq!(seq1, EventSeq(1));
        assert_eq!(seq2, EventSeq(2));
    }

    #[test]
    fn instances_have_independent_sequences() {
        let log = SqliteEventLog::open_in_memory().unwrap();
        let a = pid("inst-a");
        let b = pid("inst-b");

        log.append(Event::new(a, 1, EventKind::Snapshot, json!(null)))
            .unwrap();
        let seq_b_first = log
            .append(Event::new(b, 1, EventKind::Snapshot, json!(null)))
            .unwrap();

        assert_eq!(
            seq_b_first,
            EventSeq(1),
            "новый инстанс начинает счёт с 1 независимо от других инстансов"
        );
    }

    #[test]
    fn replay_returns_events_in_append_order_with_content_preserved() {
        let log = SqliteEventLog::open_in_memory().unwrap();
        let instance = pid("inst-order");

        log.append(Event::new(
            instance.clone(),
            3,
            EventKind::StepApplied {
                step_id: "classify".into(),
            },
            json!({"risk": 2}),
        ))
        .unwrap();
        log.append(Event::new(
            instance.clone(),
            3,
            EventKind::StepApplied {
                step_id: "fetch_card_status".into(),
            },
            json!({"status": "active"}),
        ))
        .unwrap();

        let events = log.replay(&instance).unwrap();

        assert_eq!(events.len(), 2);
        assert_eq!(events[0].seq, EventSeq(1));
        assert_eq!(events[1].seq, EventSeq(2));
        assert_eq!(
            events[0].kind,
            EventKind::StepApplied {
                step_id: "classify".into()
            }
        );
        assert_eq!(events[1].payload, json!({"status": "active"}));
    }

    #[test]
    fn replay_of_unknown_instance_is_empty_not_an_error() {
        let log = SqliteEventLog::open_in_memory().unwrap();
        let events = log.replay(&pid("never-existed")).unwrap();
        assert!(events.is_empty());
    }

    #[test]
    fn snapshot_round_trip_returns_latest() {
        let log = SqliteEventLog::open_in_memory().unwrap();
        let instance = pid("inst-snap");

        log.write_snapshot(Snapshot {
            process_instance: instance.clone(),
            seq: EventSeq(1),
            state: json!({"step": "classify"}),
        })
        .unwrap();
        log.write_snapshot(Snapshot {
            process_instance: instance.clone(),
            seq: EventSeq(5),
            state: json!({"step": "answer"}),
        })
        .unwrap();

        let latest = log
            .latest_snapshot(&instance)
            .unwrap()
            .expect("snapshot must exist");
        assert_eq!(latest.seq, EventSeq(5));
        assert_eq!(latest.state, json!({"step": "answer"}));
    }

    #[test]
    fn latest_snapshot_of_unknown_instance_is_none() {
        let log = SqliteEventLog::open_in_memory().unwrap();
        assert!(log.latest_snapshot(&pid("no-snapshot")).unwrap().is_none());
    }

    #[test]
    fn search_finds_event_by_payload_term_across_the_session_that_wrote_it() {
        let log = SqliteEventLog::open_in_memory().unwrap();
        log.append(Event::new(
            pid("inst-a"),
            1,
            EventKind::StepApplied {
                step_id: "classify".into(),
            },
            json!({"category": "billing", "summary": "Вопрос по счёту за карту"}),
        ))
        .unwrap();
        log.append(Event::new(
            pid("inst-b"),
            1,
            EventKind::StepApplied {
                step_id: "classify".into(),
            },
            json!({"category": "debt", "summary": "Просрочка платежа"}),
        ))
        .unwrap();

        let hits = log.search_episodes("billing", 10).unwrap();

        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].process_instance, pid("inst-a"));
        assert_eq!(hits[0].payload["category"], "billing");
    }

    #[test]
    fn search_across_sessions_returns_hits_from_every_matching_session() {
        let log = SqliteEventLog::open_in_memory().unwrap();
        log.append(Event::new(
            pid("inst-a"),
            1,
            EventKind::HumanGateOpened {
                reason: "высокий риск".into(),
            },
            json!(null),
        ))
        .unwrap();
        log.append(Event::new(
            pid("inst-b"),
            1,
            EventKind::HumanGateOpened {
                reason: "высокий риск повторно".into(),
            },
            json!(null),
        ))
        .unwrap();

        let hits = log.search_episodes("риск", 10).unwrap();

        let sessions: std::collections::HashSet<_> =
            hits.iter().map(|h| h.process_instance.clone()).collect();
        assert_eq!(
            sessions,
            [pid("inst-a"), pid("inst-b")].into_iter().collect()
        );
    }

    #[test]
    fn search_result_kind_and_payload_round_trip_exactly() {
        let log = SqliteEventLog::open_in_memory().unwrap();
        log.append(Event::new(
            pid("inst-rt"),
            7,
            EventKind::MediationRejected {
                reason: "схема нарушена уникальнотекст".into(),
            },
            json!({"attempt": 1}),
        ))
        .unwrap();

        let hits = log.search_episodes("уникальнотекст", 10).unwrap();

        assert_eq!(hits.len(), 1);
        assert_eq!(
            hits[0].kind,
            EventKind::MediationRejected {
                reason: "схема нарушена уникальнотекст".into()
            }
        );
        assert_eq!(hits[0].payload, json!({"attempt": 1}));
    }

    #[test]
    fn no_match_returns_empty_not_an_error() {
        let log = SqliteEventLog::open_in_memory().unwrap();
        log.append(Event::new(
            pid("inst-a"),
            1,
            EventKind::Snapshot,
            json!({"note": "что-то"}),
        ))
        .unwrap();

        assert!(log
            .search_episodes("отсутствующийтермин", 10)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn query_with_only_fts5_syntax_characters_is_empty_not_a_sql_error() {
        let log = SqliteEventLog::open_in_memory().unwrap();
        // `"`, `-`, `*` — синтаксис MATCH, не буквы/цифры: после очистки
        // от них ничего не остаётся — предсказуемый пустой результат,
        // не ошибка парсинга FTS5-запроса.
        let result = log.search_episodes("\"-*", 10);
        assert_eq!(result.unwrap(), Vec::new());
    }

    #[test]
    fn limit_caps_the_number_of_hits() {
        let log = SqliteEventLog::open_in_memory().unwrap();
        for i in 0..5 {
            log.append(Event::new(
                pid(&format!("inst-{i}")),
                1,
                EventKind::Snapshot,
                json!({"tag": "общийтермин"}),
            ))
            .unwrap();
        }

        let hits = log.search_episodes("общийтермин", 2).unwrap();
        assert_eq!(hits.len(), 2);
    }

    fn fact(id: &str, subject: &str, predicate: &str, object: &str) -> FactRecord {
        FactRecord {
            id: id.into(),
            subject: subject.into(),
            predicate: predicate.into(),
            object: object.into(),
            confidence: 0.8,
            source: "session:run-1/step:answer".into(),
            trusted_channel: true,
        }
    }

    #[test]
    fn upsert_fact_round_trips_through_all_facts() {
        let log = SqliteEventLog::open_in_memory().unwrap();
        log.upsert_fact(&fact("f-1", "клиент c-1", "живёт_в", "Москва"), None)
            .unwrap();

        let facts = log.all_facts().unwrap();

        assert_eq!(facts, vec![fact("f-1", "клиент c-1", "живёт_в", "Москва")]);
    }

    #[test]
    fn upsert_fact_updates_existing_id_not_duplicates_it() {
        let log = SqliteEventLog::open_in_memory().unwrap();
        log.upsert_fact(&fact("f-1", "клиент c-1", "живёт_в", "Москва"), None)
            .unwrap();
        log.upsert_fact(&fact("f-1", "клиент c-1", "живёт_в", "Париж"), None)
            .unwrap();

        let facts = log.all_facts().unwrap();

        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].object, "Париж");
    }

    #[test]
    fn upsert_fact_with_none_embedding_preserves_previously_stored_embedding() {
        let log = SqliteEventLog::open_in_memory().unwrap();
        log.upsert_fact(
            &fact("f-1", "клиент c-1", "живёт_в", "Москва"),
            Some(&[1.0, 0.0, 0.0]),
        )
        .unwrap();
        // Обновление без эмбеддинга не обязано его стирать.
        log.upsert_fact(&fact("f-1", "клиент c-1", "живёт_в", "Москва"), None)
            .unwrap();

        let similarity = log
            .cosine_similarity("f-1", &[1.0, 0.0, 0.0])
            .unwrap()
            .expect("эмбеддинг обязан был сохраниться");
        assert!((similarity - 1.0).abs() < 0.001);
    }

    #[test]
    fn cosine_similarity_of_identical_direction_is_one() {
        let log = SqliteEventLog::open_in_memory().unwrap();
        log.upsert_fact(
            &fact("f-1", "клиент c-1", "живёт_в", "Москва"),
            Some(&[2.0, 0.0, 0.0]),
        )
        .unwrap();

        let similarity = log
            .cosine_similarity("f-1", &[1.0, 0.0, 0.0])
            .unwrap()
            .unwrap();

        assert!((similarity - 1.0).abs() < 0.001);
    }

    #[test]
    fn cosine_similarity_of_orthogonal_vectors_is_zero() {
        let log = SqliteEventLog::open_in_memory().unwrap();
        log.upsert_fact(
            &fact("f-1", "клиент c-1", "живёт_в", "Москва"),
            Some(&[1.0, 0.0, 0.0]),
        )
        .unwrap();

        let similarity = log
            .cosine_similarity("f-1", &[0.0, 1.0, 0.0])
            .unwrap()
            .unwrap();

        assert!(similarity.abs() < 0.001);
    }

    #[test]
    fn cosine_similarity_of_unknown_fact_is_none_not_zero() {
        let log = SqliteEventLog::open_in_memory().unwrap();
        assert!(log
            .cosine_similarity("no-such-fact", &[1.0, 0.0, 0.0])
            .unwrap()
            .is_none());
    }

    #[test]
    fn cosine_similarity_of_fact_without_embedding_is_none() {
        let log = SqliteEventLog::open_in_memory().unwrap();
        log.upsert_fact(&fact("f-1", "клиент c-1", "живёт_в", "Москва"), None)
            .unwrap();

        assert!(log
            .cosine_similarity("f-1", &[1.0, 0.0, 0.0])
            .unwrap()
            .is_none());
    }

    #[test]
    fn hybrid_search_ranks_vector_and_text_match_above_vector_alone() {
        let log = SqliteEventLog::open_in_memory().unwrap();
        // f-1: и вектор, и текст совпадают с запросом.
        log.upsert_fact(
            &fact("f-1", "клиент c-1", "живёт_в", "Москва"),
            Some(&[1.0, 0.0, 0.0]),
        )
        .unwrap();
        // f-2: тот же вектор (та же векторная близость), но другой текст.
        log.upsert_fact(
            &fact("f-2", "клиент c-2", "работает_в", "офис"),
            Some(&[1.0, 0.0, 0.0]),
        )
        .unwrap();

        let hits = log.hybrid_search("Москва", &[1.0, 0.0, 0.0], 10).unwrap();

        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].fact_id, "f-1");
        assert!(hits[0].text_matched);
        assert!(hits[0].combined_score > hits[1].combined_score);
        assert!(!hits[1].text_matched);
        // Векторная близость сама по себе одинакова у обоих — разницу
        // в combined_score целиком объясняет текстовое совпадение.
        assert!((hits[0].vector_score - hits[1].vector_score).abs() < 0.001);
    }

    #[test]
    fn hybrid_search_skips_facts_without_an_embedding() {
        let log = SqliteEventLog::open_in_memory().unwrap();
        log.upsert_fact(&fact("f-1", "клиент c-1", "живёт_в", "Москва"), None)
            .unwrap();

        let hits = log.hybrid_search("Москва", &[1.0, 0.0, 0.0], 10).unwrap();

        assert!(hits.is_empty());
    }

    #[test]
    fn hybrid_search_limit_caps_results() {
        let log = SqliteEventLog::open_in_memory().unwrap();
        for i in 0..5 {
            log.upsert_fact(
                &fact(&format!("f-{i}"), "клиент c-1", "тег", &format!("v{i}")),
                Some(&[1.0, 0.0, 0.0]),
            )
            .unwrap();
        }

        let hits = log.hybrid_search("", &[1.0, 0.0, 0.0], 2).unwrap();

        assert_eq!(hits.len(), 2);
    }

    fn envelope(id: &str, from: &str, to: &str, topic: &str) -> Envelope {
        Envelope {
            id: EnvelopeId(id.into()),
            from: from.into(),
            to: to.into(),
            topic: topic.into(),
            payload: json!({"n": 1}),
        }
    }

    #[test]
    fn persisted_envelope_is_undelivered_until_marked() {
        let log = SqliteEventLog::open_in_memory().unwrap();
        log.persist_envelope(&envelope("e-1", "actor-a", "actor-b", "task.assigned"))
            .unwrap();

        let undelivered = log.undelivered_for("actor-b").unwrap();
        assert_eq!(undelivered.len(), 1);
        assert_eq!(undelivered[0].id, EnvelopeId("e-1".into()));

        log.mark_delivered(&EnvelopeId("e-1".into())).unwrap();
        assert!(log.undelivered_for("actor-b").unwrap().is_empty());
    }

    #[test]
    fn undelivered_for_only_returns_envelopes_addressed_to_that_actor() {
        let log = SqliteEventLog::open_in_memory().unwrap();
        log.persist_envelope(&envelope("e-1", "actor-a", "actor-b", "t"))
            .unwrap();
        log.persist_envelope(&envelope("e-2", "actor-a", "actor-c", "t"))
            .unwrap();

        let undelivered = log.undelivered_for("actor-b").unwrap();
        assert_eq!(undelivered.len(), 1);
        assert_eq!(undelivered[0].id, EnvelopeId("e-1".into()));
    }

    #[test]
    fn undelivered_for_unknown_actor_is_empty_not_an_error() {
        let log = SqliteEventLog::open_in_memory().unwrap();
        assert!(log.undelivered_for("no-such-actor").unwrap().is_empty());
    }

    fn one_shot(id: &str, next_fire_ms: i64) -> Schedule {
        Schedule {
            id: ScheduleId(id.into()),
            next_fire_ms,
            interval_ms: None,
            payload: json!({"kind": "one-shot"}),
        }
    }

    fn recurring(id: &str, next_fire_ms: i64, interval_ms: i64) -> Schedule {
        Schedule {
            id: ScheduleId(id.into()),
            next_fire_ms,
            interval_ms: Some(interval_ms),
            payload: json!({"kind": "recurring"}),
        }
    }

    #[test]
    fn tick_fires_due_one_shot_schedule_and_removes_it() {
        let log = SqliteEventLog::open_in_memory().unwrap();
        log.upsert_schedule(&one_shot("s-1", 1000)).unwrap();

        let fired = log.tick(1000).unwrap();
        assert_eq!(fired.len(), 1);
        assert_eq!(fired[0].id, ScheduleId("s-1".into()));

        // Одноразовое удалено — второй тик того же момента ничего не находит.
        assert!(log.tick(1000).unwrap().is_empty());
    }

    #[test]
    fn tick_ignores_schedules_not_yet_due() {
        let log = SqliteEventLog::open_in_memory().unwrap();
        log.upsert_schedule(&one_shot("s-1", 5000)).unwrap();

        assert!(log.tick(1000).unwrap().is_empty());
    }

    #[test]
    fn tick_advances_recurring_schedule_instead_of_removing_it() {
        let log = SqliteEventLog::open_in_memory().unwrap();
        log.upsert_schedule(&recurring("s-1", 1000, 500)).unwrap();

        let fired = log.tick(1000).unwrap();
        assert_eq!(fired.len(), 1);
        assert_eq!(
            fired[0].next_fire_ms, 1000,
            "сработавшее время — ДО продвижения"
        );

        // Тот же момент больше не срабатывает — next_fire_ms уже 1500.
        assert!(log.tick(1000).unwrap().is_empty());
        let fired_again = log.tick(1500).unwrap();
        assert_eq!(fired_again.len(), 1);
    }

    #[test]
    fn tick_is_protected_against_double_fire_within_the_same_moment() {
        // Защита от двойного тика: два вызова tick() с одним и тем же
        // now_ms подряд — второй не должен снова вернуть то же
        // расписание, ни для одноразового, ни для повторяющегося.
        let log = SqliteEventLog::open_in_memory().unwrap();
        log.upsert_schedule(&one_shot("s-1", 1000)).unwrap();
        log.upsert_schedule(&recurring("s-2", 1000, 100)).unwrap();

        let first = log.tick(1000).unwrap();
        let second = log.tick(1000).unwrap();

        assert_eq!(first.len(), 2);
        assert!(second.is_empty());
    }

    #[test]
    fn cancel_schedule_removes_it_before_it_ever_fires() {
        let log = SqliteEventLog::open_in_memory().unwrap();
        log.upsert_schedule(&one_shot("s-1", 1000)).unwrap();
        log.cancel_schedule(&ScheduleId("s-1".into())).unwrap();

        assert!(log.tick(1000).unwrap().is_empty());
    }

    #[test]
    fn upsert_schedule_updates_existing_id_not_duplicates_it() {
        let log = SqliteEventLog::open_in_memory().unwrap();
        log.upsert_schedule(&one_shot("s-1", 1000)).unwrap();
        log.upsert_schedule(&one_shot("s-1", 2000)).unwrap();

        assert!(log.tick(1000).unwrap().is_empty());
        assert_eq!(log.tick(2000).unwrap().len(), 1);
    }

    #[test]
    fn due_finds_a_ready_schedule_without_consuming_it() {
        let log = SqliteEventLog::open_in_memory().unwrap();
        log.upsert_schedule(&one_shot("s-1", 1000)).unwrap();

        let peeked = log.due(1000).unwrap();
        assert_eq!(peeked.len(), 1);
        assert_eq!(peeked[0].id, ScheduleId("s-1".into()));

        // due() не продвинул и не удалил расписание — tick() всё ещё его находит.
        let fired = log.tick(1000).unwrap();
        assert_eq!(fired.len(), 1);
    }

    #[test]
    fn due_ignores_schedules_not_yet_due() {
        let log = SqliteEventLog::open_in_memory().unwrap();
        log.upsert_schedule(&one_shot("s-1", 5000)).unwrap();

        assert!(log.due(1000).unwrap().is_empty());
    }

    #[test]
    fn due_can_be_called_repeatedly_without_side_effects() {
        let log = SqliteEventLog::open_in_memory().unwrap();
        log.upsert_schedule(&recurring("s-1", 1000, 500)).unwrap();

        let first = log.due(1000).unwrap();
        let second = log.due(1000).unwrap();

        assert_eq!(first, second);
    }

    fn node(id: &str, node_type: &str) -> NodeRecord {
        NodeRecord {
            id: id.into(),
            node_type: node_type.into(),
            properties: json!({"batch_number": "B-1"}),
        }
    }

    fn edge(id: &str, edge_type: &str, source: &str, target: &str) -> EdgeRecord {
        EdgeRecord {
            id: id.into(),
            edge_type: edge_type.into(),
            source: source.into(),
            target: target.into(),
            properties: json!({"since": "2026-01-01"}),
        }
    }

    #[test]
    fn upsert_node_round_trips_through_all_nodes() {
        let log = SqliteEventLog::open_in_memory().unwrap();
        log.upsert_node(&node("n-1", "batch")).unwrap();

        let nodes = log.all_nodes().unwrap();

        assert_eq!(nodes, vec![node("n-1", "batch")]);
    }

    #[test]
    fn upsert_node_updates_existing_id_not_duplicates_it() {
        let log = SqliteEventLog::open_in_memory().unwrap();
        log.upsert_node(&node("n-1", "batch")).unwrap();
        log.upsert_node(&NodeRecord {
            properties: json!({"batch_number": "B-2"}),
            ..node("n-1", "batch")
        })
        .unwrap();

        let nodes = log.all_nodes().unwrap();

        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].properties, json!({"batch_number": "B-2"}));
    }

    #[test]
    fn all_nodes_of_empty_store_is_empty_not_an_error() {
        let log = SqliteEventLog::open_in_memory().unwrap();
        assert!(log.all_nodes().unwrap().is_empty());
    }

    #[test]
    fn upsert_edge_round_trips_through_all_edges() {
        let log = SqliteEventLog::open_in_memory().unwrap();
        log.upsert_edge(&edge("e-1", "supplied_by", "n-1", "n-2"))
            .unwrap();

        let edges = log.all_edges().unwrap();

        assert_eq!(edges, vec![edge("e-1", "supplied_by", "n-1", "n-2")]);
    }

    #[test]
    fn upsert_edge_updates_existing_id_not_duplicates_it() {
        let log = SqliteEventLog::open_in_memory().unwrap();
        log.upsert_edge(&edge("e-1", "supplied_by", "n-1", "n-2"))
            .unwrap();
        log.upsert_edge(&edge("e-1", "supplied_by", "n-1", "n-3"))
            .unwrap();

        let edges = log.all_edges().unwrap();

        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].target, "n-3");
    }

    #[test]
    fn all_edges_of_empty_store_is_empty_not_an_error() {
        let log = SqliteEventLog::open_in_memory().unwrap();
        assert!(log.all_edges().unwrap().is_empty());
    }
}
