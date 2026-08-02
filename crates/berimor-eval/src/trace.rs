//! Трассировка и replay событий инстанса (O1).
//!
//! Источник: `ideal-agent-architecture.md` §3.11 («каждый шаг — событие;
//! любой прогон воспроизводится из журнала»). ROADMAP: O1.
//!
//! Отличие от `berimor_process_engine::engine::recover`: `recover`
//! восстанавливает исполняемый `ProcessInstance` (нужен граф процесса,
//! проверка версии — цель «продолжить выполнение»); этот модуль —
//! только чтение журнала для человека/отладки, графа не требует и не
//! ограничен последним событием — можно посмотреть состояние на ЛЮБОЙ
//! момент («каким было состояние перед тем, как шаг X провалился»).

use berimor_process_engine::state::fold;
use berimor_storage::{EventLog, StorageError};
use berimor_types::event::{Event, EventKind, EventSeq, ProcessInstanceId};
use serde_json::Value;

/// Одно событие журнала в человекочитаемом виде.
#[derive(Debug, Clone, PartialEq)]
pub struct TraceEntry {
    pub seq: EventSeq,
    pub ts_ms: i64,
    /// Машиночитаемый вид `EventKind` (`snake_case`, без данных) — для
    /// фильтрации/группировки, не для показа человеку.
    pub kind: &'static str,
    /// Краткое описание для человека, включает данные события там, где они есть.
    pub summary: String,
}

fn describe(event: &Event) -> TraceEntry {
    let (kind, summary) = match &event.kind {
        EventKind::Instantiated => ("instantiated", "инстанс создан".to_string()),
        EventKind::StepApplied { step_id } => ("step_applied", format!("шаг '{step_id}' применён")),
        EventKind::ParallelStepApplied {
            fork_step_id,
            branch_step_id,
        } => (
            "parallel_step_applied",
            format!("ветвь '{branch_step_id}' форка '{fork_step_id}' применена"),
        ),
        EventKind::MediationParsed => ("mediation_parsed", "вывод модели разобран".to_string()),
        EventKind::MediationValidated => (
            "mediation_validated",
            "вывод модели прошёл валидацию".to_string(),
        ),
        EventKind::MediationCommitted => (
            "mediation_committed",
            "вывод модели зафиксирован как патч".to_string(),
        ),
        EventKind::MediationRejected { reason } => (
            "mediation_rejected",
            format!("вывод модели отклонён: {reason}"),
        ),
        EventKind::HumanGateOpened { reason } => (
            "human_gate_opened",
            format!("остановка на подтверждение: {reason}"),
        ),
        EventKind::HumanGateResolved => (
            "human_gate_resolved",
            "подтверждение получено, выполнение продолжено".to_string(),
        ),
        EventKind::HumanGateTimedOut { policy } => (
            "human_gate_timed_out",
            format!("таймаут ожидания подтверждения, политика '{policy}'"),
        ),
        EventKind::MemoryConflict { detail } => (
            "memory_conflict",
            format!("конфликт фактов памяти: {detail}"),
        ),
        EventKind::Snapshot => ("snapshot", "снапшот состояния записан".to_string()),
        EventKind::SecurityEvent { detail } => {
            ("security_event", format!("событие безопасности: {detail}"))
        }
        EventKind::VersionMigrated {
            from_version,
            to_version,
        } => (
            "version_migrated",
            format!("инстанс переведён с версии {from_version} на {to_version}"),
        ),
        EventKind::TrustListChanged { action, repo, .. } => (
            "trust_list_changed",
            format!("доверенный список: {action:?} '{repo}'"),
        ),
    };
    TraceEntry {
        seq: event.seq,
        ts_ms: event.ts_ms,
        kind,
        summary,
    }
}

/// Полная трассировка инстанса — все события журнала по порядку `seq`, в
/// человекочитаемом виде. Неизвестный инстанс — пустая трассировка, не
/// ошибка (то же соглашение, что у `EventLog::replay`).
pub fn trace(
    log: &dyn EventLog,
    process_instance: &ProcessInstanceId,
) -> Result<Vec<TraceEntry>, StorageError> {
    let events = log.replay(process_instance)?;
    Ok(events.iter().map(describe).collect())
}

/// Состояние инстанса на момент `seq` ВКЛЮЧИТЕЛЬНО — не обязательно
/// последнее событие журнала. `seq`, которого нет в журнале (в будущем
/// относительно последнего события), даёт то же состояние, что и
/// последнее реальное событие — та же семантика, что у среза `..=seq`.
pub fn replay_until(
    log: &dyn EventLog,
    process_instance: &ProcessInstanceId,
    seq: EventSeq,
) -> Result<Value, StorageError> {
    let events = log.replay(process_instance)?;
    let prefix: Vec<Event> = events.into_iter().take_while(|e| e.seq <= seq).collect();
    Ok(fold(&prefix))
}

#[cfg(test)]
mod tests {
    use super::*;
    use berimor_storage::SqliteEventLog;
    use serde_json::json;

    fn append(
        log: &SqliteEventLog,
        id: &ProcessInstanceId,
        kind: EventKind,
        payload: Value,
    ) -> EventSeq {
        log.append(Event::new(id.clone(), 1, kind, payload))
            .unwrap()
    }

    #[test]
    fn trace_of_unknown_instance_is_empty_not_an_error() {
        let log = SqliteEventLog::open_in_memory().unwrap();
        let id = ProcessInstanceId("no-such-instance".into());

        assert!(trace(&log, &id).unwrap().is_empty());
    }

    #[test]
    fn trace_describes_events_in_order_with_human_readable_summaries() {
        let log = SqliteEventLog::open_in_memory().unwrap();
        let id = ProcessInstanceId("inst-1".into());
        append(
            &log,
            &id,
            EventKind::Instantiated,
            json!({"card_id": "c-1"}),
        );
        append(
            &log,
            &id,
            EventKind::StepApplied {
                step_id: "classify".into(),
            },
            json!({}),
        );
        append(
            &log,
            &id,
            EventKind::MediationRejected {
                reason: "поле 'risk' отсутствует".into(),
            },
            json!({}),
        );

        let entries = trace(&log, &id).unwrap();

        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].kind, "instantiated");
        assert_eq!(entries[1].kind, "step_applied");
        assert_eq!(entries[1].summary, "шаг 'classify' применён");
        assert_eq!(entries[2].kind, "mediation_rejected");
        assert!(entries[2].summary.contains("поле 'risk' отсутствует"));
        assert!(entries[0].seq < entries[1].seq);
        assert!(entries[1].seq < entries[2].seq);
    }

    #[test]
    fn replay_until_reconstructs_state_at_an_earlier_point_not_just_the_latest() {
        let log = SqliteEventLog::open_in_memory().unwrap();
        let id = ProcessInstanceId("inst-1".into());
        append(
            &log,
            &id,
            EventKind::Instantiated,
            json!({"card_id": "c-1"}),
        );
        let after_first_step = append(
            &log,
            &id,
            EventKind::StepApplied {
                step_id: "classify".into(),
            },
            json!({"risk": "low"}),
        );
        append(
            &log,
            &id,
            EventKind::StepApplied {
                step_id: "route".into(),
            },
            json!({"target": "auto"}),
        );

        let state_after_first_step = replay_until(&log, &id, after_first_step).unwrap();

        assert!(
            state_after_first_step.get("classify").is_some(),
            "первый шаг уже применён"
        );
        assert!(
            state_after_first_step.get("route").is_none(),
            "второй шаг ещё не применён на этот момент"
        );
    }

    #[test]
    fn replay_until_the_final_seq_matches_full_fold() {
        let log = SqliteEventLog::open_in_memory().unwrap();
        let id = ProcessInstanceId("inst-1".into());
        append(
            &log,
            &id,
            EventKind::Instantiated,
            json!({"card_id": "c-1"}),
        );
        let last = append(
            &log,
            &id,
            EventKind::StepApplied {
                step_id: "classify".into(),
            },
            json!({"risk": "low"}),
        );

        let full_state = replay_until(&log, &id, last).unwrap();

        assert_eq!(full_state["classify"]["risk"], json!("low"));
    }
}
