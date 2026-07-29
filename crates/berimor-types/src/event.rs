//! Событие и снапшот журнала.
//!
//! Источник: `arch/process-engine.md` §3 («Состояние»), `arch/views/data-architecture.md` §1.
//! ROADMAP: F1, F2.

use serde::{Deserialize, Serialize};

/// Идентификатор инстанса процесса.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ProcessInstanceId(pub String);

/// Порядковый номер события в журнале инстанса — монотонно возрастает.
/// Свёртка событий `0..=seq` детерминированно восстанавливает состояние
/// (инвариант I7: «каждый шаг воспроизводим»).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct EventSeq(pub u64);

/// Одна неизменяемая запись в журнале. `process-engine.md` §3:
/// «каждый патч = событие `step.applied` в журнале».
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub seq: EventSeq,
    pub process_instance: ProcessInstanceId,
    /// Версия графа процесса на момент события — инстанс привязан к ней
    /// на весь жизненный цикл (ADR-0012), восстановление идёт по ней же.
    pub process_version: u32,
    pub kind: EventKind,
    pub payload: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EventKind {
    StepApplied { step_id: String },
    MediationParsed,
    MediationValidated,
    MediationCommitted,
    MediationRejected { reason: String },
    HumanGateOpened { reason: String },
    HumanGateResolved,
    Snapshot,
    SecurityEvent { detail: String },
}

/// Материализованный кэш свёртки на момент `seq`. Ускоряет восстановление,
/// но не является источником истины — им остаётся журнал (`process-engine.md` §4).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub process_instance: ProcessInstanceId,
    pub seq: EventSeq,
    pub state: serde_json::Value,
}
