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
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        Self::from_connection(conn)
    }

    /// In-memory журнал — для тестов и для Milestone 0 (`docs/ROADMAP.md` §3).
    /// WAL для `:memory:` не имеет смысла (нет файла) — не включается.
    pub fn open_in_memory() -> Result<Self, StorageError> {
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
}
