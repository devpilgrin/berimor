//! Событие и снапшот журнала.
//!
//! Источник: `docs/arch/process-engine.md` §3 («Состояние»), `docs/arch/views/data-architecture.md` §1.
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
///
/// `seq` и `ts_ms` не заполняются вызывающим кодом — их атомарно
/// присваивает хранилище при [`append`](../berimor_storage/trait.EventLog.html#tymethod.append),
/// это исключает гонку между писателями за номер следующего события.
/// Используйте [`Event::new`], а не литерал структуры.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub seq: EventSeq,
    pub process_instance: ProcessInstanceId,
    /// Версия графа процесса на момент события — инстанс привязан к ней
    /// на весь жизненный цикл (ADR-0012), восстановление идёт по ней же.
    pub process_version: u32,
    pub kind: EventKind,
    pub payload: serde_json::Value,
    /// Unix-время в миллисекундах на момент записи. Часть аудит-следа
    /// (`security-model.md` §5: «кто, что, когда»), не участвует в свёртке
    /// состояния — только `kind`/`payload` определяют патч.
    pub ts_ms: i64,
}

impl Event {
    /// `seq: EventSeq(0)` и `ts_ms: 0` — заглушки, хранилище их не читает,
    /// только перезаписывает своими значениями при `append`.
    pub fn new(
        process_instance: ProcessInstanceId,
        process_version: u32,
        kind: EventKind,
        payload: serde_json::Value,
    ) -> Self {
        Self {
            seq: EventSeq(0),
            process_instance,
            process_version,
            kind,
            payload,
            ts_ms: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum EventKind {
    /// Первое событие инстанса: `payload` — исходный `input`, переданный в
    /// `instantiate`. Без этого события `fold` не может восстановить
    /// состояние целиком после сбоя — вход процесса теряется, поскольку он
    /// иначе нигде не журналируется (найдено на P3, `docs/ROADMAP.md`).
    Instantiated,
    StepApplied {
        step_id: String,
    },
    MediationParsed,
    MediationValidated,
    MediationCommitted,
    MediationRejected {
        reason: String,
    },
    HumanGateOpened {
        reason: String,
    },
    HumanGateResolved,
    /// P7: истёк таймаут ожидания ответа человека — `policy` дублирует
    /// вид примененной политики (`"fail"`/`"branch"`/`"escalate"`) для
    /// аудита без обращения к декларации процесса.
    HumanGateTimedOut {
        policy: String,
    },
    Snapshot,
    SecurityEvent {
        detail: String,
    },
    /// P8 (ADR-0012): инстанс явно переведён на новую версию графа
    /// операцией `migrate_version` — аудит-след «кто, когда, с какой
    /// версии на какую», не патч состояния (свёртка это событие
    /// игнорирует, как и все остальные не-`StepApplied`/`Instantiated`).
    VersionMigrated {
        from_version: u32,
        to_version: u32,
    },
}

/// Материализованный кэш свёртки на момент `seq`. Ускоряет восстановление,
/// но не является источником истины — им остаётся журнал (`process-engine.md` §4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub process_instance: ProcessInstanceId,
    pub seq: EventSeq,
    pub state: serde_json::Value,
}
