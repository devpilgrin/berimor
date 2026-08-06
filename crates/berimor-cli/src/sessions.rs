//! §20.22 v2, шаг 1: реестр живых сессий хоста на общем журнале
//! (`docs/design-swarm-sessions.md`).
//!
//! События `SessionOpened/Heartbeat/Closed` пишутся под синтетическим
//! инстансом `host-sessions` (прецедент — `trust-list`). «Мёртвая»
//! сессия определяется читателем: есть `SessionClosed` ИЛИ pid ушёл
//! из /proc (журнал per-host — pid-проверка легитимна). Heartbeat —
//! на границе хода (REPL) / тика (daemon), без таймерных потоков.

use berimor_storage::{EventLog, SqliteEventLog, StorageError};
use berimor_types::event::{Event, EventKind, ProcessInstanceId};
use serde::Serialize;

pub const SESSIONS_INSTANCE_ID: &str = "host-sessions";

/// `sess-<pid>-<ms>` — читаемый и уникальный на хосте (pid + время).
pub fn new_session_id() -> String {
    format!("sess-{}-{}", std::process::id(), now_ms())
}

fn journal_event(kind: EventKind) -> Event {
    Event::new(
        ProcessInstanceId(SESSIONS_INSTANCE_ID.to_string()),
        0,
        kind,
        serde_json::Value::Null,
    )
}

/// Открытие сессии — вызывается на старте chat/run/daemon.
pub fn record_open(
    journal: &SqliteEventLog,
    session_id: &str,
    command: &str,
) -> Result<(), StorageError> {
    journal.append(journal_event(EventKind::SessionOpened {
        session_id: session_id.to_string(),
        pid: std::process::id(),
        cwd: std::env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| ".".into()),
        command: command.to_string(),
    }))?;
    Ok(())
}

pub fn record_heartbeat(journal: &SqliteEventLog, session_id: &str) -> Result<(), StorageError> {
    journal.append(journal_event(EventKind::SessionHeartbeat {
        session_id: session_id.to_string(),
    }))?;
    Ok(())
}

pub fn record_closed(journal: &SqliteEventLog, session_id: &str) -> Result<(), StorageError> {
    journal.append(journal_event(EventKind::SessionClosed {
        session_id: session_id.to_string(),
    }))?;
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SessionInfo {
    pub session_id: String,
    pub pid: u32,
    pub cwd: String,
    pub command: String,
    /// ts_ms открытия и последнего события (heartbeat/closed) — из журнала.
    pub opened_ts_ms: i64,
    pub last_ts_ms: i64,
    pub closed: bool,
    /// pid присутствует в /proc (локальная проверка читателя).
    pub pid_alive: bool,
}

fn pid_alive(pid: u32) -> bool {
    std::path::Path::new(&format!("/proc/{pid}")).exists()
}

/// Свёртка реестра: последнее состояние на session_id.
pub fn fold_sessions(events: &[Event]) -> Vec<SessionInfo> {
    let mut order: Vec<String> = Vec::new();
    let mut map: std::collections::HashMap<String, SessionInfo> = Default::default();
    for event in events {
        match &event.kind {
            EventKind::SessionOpened {
                session_id,
                pid,
                cwd,
                command,
            } => {
                if !map.contains_key(session_id) {
                    order.push(session_id.clone());
                }
                map.insert(
                    session_id.clone(),
                    SessionInfo {
                        session_id: session_id.clone(),
                        pid: *pid,
                        cwd: cwd.clone(),
                        command: command.clone(),
                        opened_ts_ms: event.ts_ms,
                        last_ts_ms: event.ts_ms,
                        closed: false,
                        pid_alive: pid_alive(*pid),
                    },
                );
            }
            EventKind::SessionHeartbeat { session_id } => {
                if let Some(info) = map.get_mut(session_id) {
                    info.last_ts_ms = event.ts_ms;
                }
            }
            EventKind::SessionClosed { session_id } => {
                if let Some(info) = map.get_mut(session_id) {
                    info.closed = true;
                    info.last_ts_ms = event.ts_ms;
                }
            }
            _ => {}
        }
    }
    // pid живёт — проверка на момент чтения, не на момент открытия.
    for info in map.values_mut() {
        info.pid_alive = pid_alive(info.pid);
    }
    order.into_iter().filter_map(|id| map.remove(&id)).collect()
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// `berimor sessions` — живые сессии хоста (не закрытые + pid жив).
pub fn cmd_sessions(config: &crate::config::Config) -> Result<(), String> {
    let journal = SqliteEventLog::open(&config.storage_path).map_err(|e| format!("журнал: {e}"))?;
    let events = journal
        .replay(&ProcessInstanceId(SESSIONS_INSTANCE_ID.to_string()))
        .map_err(|e| format!("журнал: {e}"))?;
    let sessions = fold_sessions(&events);
    let live: Vec<_> = sessions
        .iter()
        .filter(|s| !s.closed && s.pid_alive)
        .collect();
    if live.is_empty() {
        println!(
            "[berimor] живых сессий нет (завершённых в реестре: {})",
            sessions.len()
        );
        return Ok(());
    }
    for s in &live {
        println!(
            "{} | {} | pid {} | {} | heartbeat {} мс назад",
            s.session_id,
            s.command,
            s.pid,
            s.cwd,
            now_ms() - s.last_ts_ms
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Конвенция daemon.rs-тестов: temp_dir + pid, без tempfile-зависимости.
    fn temp_journal(tag: &str) -> (std::path::PathBuf, SqliteEventLog) {
        let dir = std::env::temp_dir().join(format!(
            "berimor-sessions-test-{tag}-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("tempdir");
        let journal = SqliteEventLog::open(&dir.join("sessions.db")).expect("open");
        (dir, journal)
    }

    #[test]
    fn registry_tracks_open_heartbeat_close() {
        let (_dir, journal) = temp_journal("fold");
        record_open(&journal, "sess-a", "chat").expect("open");
        record_heartbeat(&journal, "sess-a").expect("heartbeat");
        record_open(&journal, "sess-b", "daemon").expect("open b");
        record_closed(&journal, "sess-b").expect("close b");
        let events = journal
            .replay(&ProcessInstanceId(SESSIONS_INSTANCE_ID.into()))
            .expect("replay");
        let sessions = fold_sessions(&events);
        assert_eq!(sessions.len(), 2);
        let a = sessions
            .iter()
            .find(|s| s.session_id == "sess-a")
            .expect("a");
        let b = sessions
            .iter()
            .find(|s| s.session_id == "sess-b")
            .expect("b");
        assert!(!a.closed && a.command == "chat");
        assert!(a.last_ts_ms >= a.opened_ts_ms);
        assert!(b.closed && b.command == "daemon");
    }

    #[test]
    fn live_filter_excludes_closed_and_dead_pids() {
        let (_dir, journal) = temp_journal("live");
        record_open(&journal, "sess-live", "chat").expect("open");
        // Сессия с заведомо мёртвым pid: журналируем вручную.
        journal
            .append(journal_event(EventKind::SessionOpened {
                session_id: "sess-dead".into(),
                pid: 4_000_000,
                cwd: "/tmp".into(),
                command: "chat".into(),
            }))
            .expect("open dead");
        let events = journal
            .replay(&ProcessInstanceId(SESSIONS_INSTANCE_ID.into()))
            .expect("replay");
        let sessions = fold_sessions(&events);
        let live: Vec<_> = sessions
            .iter()
            .filter(|s| !s.closed && s.pid_alive)
            .collect();
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].session_id, "sess-live");
    }
}
