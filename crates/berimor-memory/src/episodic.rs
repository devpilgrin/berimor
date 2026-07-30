//! Эпизодическая память: журнал навсегда, полнотекстовый индекс (FTS5).
//!
//! Источник: `docs/arch/memory-model.md` §1. ROADMAP: MEM2.
//!
//! Индекс и примитив поиска — в `berimor-storage` (тот же журнал, что и
//! источник истины движка процесса, ADR-0021: «одно хранилище, не
//! четыре»). Этот модуль добавляет доменную форму результата: «поиск по
//! сессиям» (§1) буквально группирует совпадения по сессии
//! (`process_instance_id`), а не отдаёт плоский список без группировки —
//! вызывающему коду (будущему слою `Session` Context Engine, Фаза 3)
//! нужны сессии-кандидаты, не отдельные события сами по себе.

use berimor_storage::{EpisodeHit, EpisodicSearch, StorageError};
use berimor_types::event::ProcessInstanceId;

/// Совпадения одной сессии в порядке, в котором их вернул поиск
/// (по релевантности, FTS5 `rank`).
#[derive(Debug, Clone, PartialEq)]
pub struct SessionMatch {
    pub session: ProcessInstanceId,
    pub hits: Vec<EpisodeHit>,
}

/// Группирует плоский ранжированный результат
/// [`EpisodicSearch::search_episodes`] по сессии, сохраняя порядок первого
/// (самого релевантного) появления сессии — сессия с более релевантным
/// совпадением идёт раньше, даже если совпадений у неё меньше, чем у
/// другой.
pub fn search_sessions(
    index: &dyn EpisodicSearch,
    query: &str,
    limit: usize,
) -> Result<Vec<SessionMatch>, StorageError> {
    let hits = index.search_episodes(query, limit)?;
    let mut sessions: Vec<SessionMatch> = Vec::new();
    for hit in hits {
        match sessions
            .iter_mut()
            .find(|s| s.session == hit.process_instance)
        {
            Some(existing) => existing.hits.push(hit),
            None => sessions.push(SessionMatch {
                session: hit.process_instance.clone(),
                hits: vec![hit],
            }),
        }
    }
    Ok(sessions)
}

#[cfg(test)]
mod tests {
    use super::*;
    use berimor_storage::{EventLog, SqliteEventLog};
    use berimor_types::event::{Event, EventKind};
    use serde_json::json;

    fn pid(s: &str) -> ProcessInstanceId {
        ProcessInstanceId(s.to_string())
    }

    #[test]
    fn groups_hits_by_session_preserving_relevance_order() {
        let log = SqliteEventLog::open_in_memory().unwrap();
        log.append(Event::new(
            pid("a"),
            1,
            EventKind::Snapshot,
            json!({"note": "искомый термин один"}),
        ))
        .unwrap();
        log.append(Event::new(
            pid("a"),
            1,
            EventKind::Snapshot,
            json!({"note": "искомый термин два"}),
        ))
        .unwrap();
        log.append(Event::new(
            pid("b"),
            1,
            EventKind::Snapshot,
            json!({"note": "искомый термин три"}),
        ))
        .unwrap();

        let sessions = search_sessions(&log, "искомый", 10).unwrap();

        assert_eq!(sessions.len(), 2);
        let a = sessions.iter().find(|s| s.session == pid("a")).unwrap();
        assert_eq!(a.hits.len(), 2);
        let b = sessions.iter().find(|s| s.session == pid("b")).unwrap();
        assert_eq!(b.hits.len(), 1);
    }

    #[test]
    fn no_match_is_an_empty_session_list() {
        let log = SqliteEventLog::open_in_memory().unwrap();
        assert!(search_sessions(&log, "нет-такого-термина-нигде", 10)
            .unwrap()
            .is_empty());
    }

    /// Композиция: сессия из golden-фикстуры находится по тексту решения
    /// human_gate — то, что реально пишет `berimor run` в журнал (CLI2).
    #[test]
    fn finds_session_by_human_gate_reason_text() {
        let log = SqliteEventLog::open_in_memory().unwrap();
        log.append(Event::new(
            pid("run-1"),
            3,
            EventKind::HumanGateOpened {
                reason: "высокий риск: 8".into(),
            },
            json!(null),
        ))
        .unwrap();

        let sessions = search_sessions(&log, "высокий риск", 10).unwrap();

        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].session, pid("run-1"));
    }
}
