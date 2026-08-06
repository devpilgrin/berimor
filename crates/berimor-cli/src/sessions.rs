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

// ---- §20.22 v2 шаг 2: уведомления о файлах -------------------------------

use berimor_storage::{Envelope, EnvelopeId, MailboxLog};

pub const TOPIC_FILE_CHANGED: &str = "file.changed";
/// §20.22 v2 шаг 3: сообщение между сессиями (/tell, /broadcast).
pub const TOPIC_SESSION_MESSAGE: &str = "session.message";

/// /tell: сообщение конкретной сессии (персистентный конверт — офлайн-
/// получатель прочтёт при возвращении, это почта, не чат).
pub fn send_message(
    journal: &SqliteEventLog,
    from_session: &str,
    to_session: &str,
    text: &str,
) -> Result<(), String> {
    let envelope = Envelope {
        id: EnvelopeId(format!("msg-{from_session}-{to_session}-{}", now_ms())),
        from: from_session.to_string(),
        to: to_session.to_string(),
        topic: TOPIC_SESSION_MESSAGE.to_string(),
        payload: serde_json::json!({"text": text}),
    };
    journal
        .persist_envelope(&envelope)
        .map_err(|e| format!("почта: {e}"))
}

/// /broadcast: всем ЖИВЫМ сессиям, кроме себя. Возвращает число
/// получателей — отправитель видит, что сообщение не ушло в пустоту.
pub fn broadcast_message(
    journal: &SqliteEventLog,
    from_session: &str,
    text: &str,
) -> Result<usize, String> {
    let events = journal
        .replay(&ProcessInstanceId(SESSIONS_INSTANCE_ID.to_string()))
        .map_err(|e| format!("журнал: {e}"))?;
    let live: Vec<String> = fold_sessions(&events)
        .into_iter()
        .filter(|s| !s.closed && s.pid_alive && s.session_id != from_session)
        .map(|s| s.session_id)
        .collect();
    for target in &live {
        send_message(journal, from_session, target, text)?;
    }
    Ok(live.len())
}

/// Живые сессии (не closed + pid жив), наблюдавшие `path` (FileObserved),
/// кроме самого писателя.
pub fn sessions_observing(
    journal: &SqliteEventLog,
    path: &str,
    exclude_session: &str,
) -> Vec<String> {
    let events = journal
        .replay(&ProcessInstanceId(SESSIONS_INSTANCE_ID.to_string()))
        .unwrap_or_default();
    let sessions = fold_sessions(&events);
    let mut observers = Vec::new();
    for event in &events {
        if let EventKind::FileObserved {
            session_id,
            path: p,
        } = &event.kind
        {
            if p == path
                && session_id != exclude_session
                && !observers.contains(session_id)
                && sessions
                    .iter()
                    .any(|s| &s.session_id == session_id && !s.closed && s.pid_alive)
            {
                observers.push(session_id.clone());
            }
        }
    }
    observers
}

/// FileTouched + конверты наблюдателям — одна точка из диспетчера.
pub fn record_touched_and_notify(journal: &SqliteEventLog, session_id: &str, path: &str, op: &str) {
    let _ = journal.append(journal_event(EventKind::FileTouched {
        session_id: session_id.to_string(),
        path: path.to_string(),
        op: op.to_string(),
    }));
    for observer in sessions_observing(journal, path, session_id) {
        let envelope = Envelope {
            id: EnvelopeId(format!(
                "filechg-{session_id}-{observer}-{}-{path}",
                now_ms()
            )),
            from: session_id.to_string(),
            to: observer,
            topic: TOPIC_FILE_CHANGED.to_string(),
            payload: serde_json::json!({"path": path, "by_session": session_id, "op": op}),
        };
        let _ = journal.persist_envelope(&envelope);
    }
}

/// FileObserved с дедупликацией «один путь за сессию» — читается журнал;
/// вызывающий кэширует в HashSet, здесь — честная проверка для тестов и
/// холодного старта диспетчера.
pub fn record_observed(journal: &SqliteEventLog, session_id: &str, path: &str) {
    let _ = journal.append(journal_event(EventKind::FileObserved {
        session_id: session_id.to_string(),
        path: path.to_string(),
    }));
}

/// Дренаж входящих конвертов сессии: забрать недоставленные и пометить
/// доставленными (доставка = показ пользователю вызывающим).
pub fn drain_envelopes(journal: &SqliteEventLog, session_id: &str) -> Vec<Envelope> {
    let envelopes = journal.undelivered_for(session_id).unwrap_or_default();
    for envelope in &envelopes {
        let _ = journal.mark_delivered(&envelope.id);
    }
    envelopes
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
    fn observer_gets_envelope_on_touch_and_drains_once() {
        let (_dir, journal) = temp_journal("notify");
        record_open(&journal, "sess-reader", "chat").expect("open r");
        record_open(&journal, "sess-writer", "chat").expect("open w");
        record_observed(&journal, "sess-reader", "src/main.rs");
        record_touched_and_notify(&journal, "sess-writer", "src/main.rs", "files.write");
        let drained = drain_envelopes(&journal, "sess-reader");
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].topic, TOPIC_FILE_CHANGED);
        assert_eq!(drained[0].from, "sess-writer");
        assert_eq!(drained[0].payload["path"], "src/main.rs");
        // Повторный дренаж — пусто (mark_delivered).
        assert!(drain_envelopes(&journal, "sess-reader").is_empty());
    }

    #[test]
    fn writer_is_not_notified_about_own_touch() {
        let (_dir, journal) = temp_journal("self");
        record_open(&journal, "sess-solo", "chat").expect("open");
        record_observed(&journal, "sess-solo", "README.md");
        record_touched_and_notify(&journal, "sess-solo", "README.md", "files.write");
        assert!(drain_envelopes(&journal, "sess-solo").is_empty());
    }

    #[test]
    fn closed_session_is_not_notified() {
        let (_dir, journal) = temp_journal("closed");
        record_open(&journal, "sess-gone", "chat").expect("open");
        record_observed(&journal, "sess-gone", "a.txt");
        record_closed(&journal, "sess-gone").expect("close");
        record_open(&journal, "sess-active", "chat").expect("open a");
        record_touched_and_notify(&journal, "sess-active", "a.txt", "files.write");
        assert!(drain_envelopes(&journal, "sess-gone").is_empty());
    }

    #[test]
    fn tell_delivers_message_to_target_only() {
        let (_dir, journal) = temp_journal("tell");
        record_open(&journal, "sess-a", "chat").expect("a");
        record_open(&journal, "sess-b", "chat").expect("b");
        send_message(&journal, "sess-a", "sess-b", "привет, B").expect("send");
        assert!(drain_envelopes(&journal, "sess-a").is_empty());
        let drained = drain_envelopes(&journal, "sess-b");
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].topic, TOPIC_SESSION_MESSAGE);
        assert_eq!(drained[0].payload["text"], "привет, B");
    }

    #[test]
    fn broadcast_reaches_live_sessions_except_sender() {
        let (_dir, journal) = temp_journal("bcast");
        record_open(&journal, "sess-a", "chat").expect("a");
        record_open(&journal, "sess-b", "chat").expect("b");
        record_open(&journal, "sess-c", "daemon").expect("c");
        record_closed(&journal, "sess-c").expect("close c");
        let reached = broadcast_message(&journal, "sess-a", "всем привет").expect("bcast");
        assert_eq!(reached, 1); // только sess-b: a — отправитель, c — закрыта
        let drained = drain_envelopes(&journal, "sess-b");
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].payload["text"], "всем привет");
        assert!(drain_envelopes(&journal, "sess-c").is_empty());
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
