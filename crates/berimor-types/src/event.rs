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
    /// P5: патч ОДНОЙ ветви parallel-шага — пишется в изолированный
    /// неймспейс `state.parallel.<fork_step_id>.<branch_step_id>`
    /// (`process-engine.md` §4), не в `state.<branch_step_id>` напрямую:
    /// одноимённый шаг вне parallel-контекста не должен быть перезаписан
    /// или перезаписать результат ветви.
    ParallelStepApplied {
        fork_step_id: String,
        branch_step_id: String,
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
    /// Предложенный моделью факт противоречит уже сохранённому (тот же
    /// субъект и предикат, другой объект) — запись НЕ выполнена, решение
    /// остаётся за человеком (memory-model.md §2, «не молчаливая
    /// перезапись»). Журнал, не блокирование процесса: извлечение —
    /// фоновая операция после Finished, спрашивать некогда.
    MemoryConflict {
        detail: String,
    },
    /// `berimor memory consolidate` (prompt-next-wave.md задача 3): группа
    /// семантически близких фактов слита в один — `detail` перечисляет
    /// survivor и поглощённые id с оценками схожести (тот же стиль, что
    /// `MemoryConflict::detail` — человекочитаемая строка, не структура:
    /// это аудиторский след для чтения, не машинно разбираемый контракт).
    /// Журналуется под синтетическим `ProcessInstanceId("memory-consolidation")`
    /// (тот же приём, что `"trust-list"`/`"host-sessions"` — консолидация
    /// не принадлежит ни одному прогону процесса).
    FactsConsolidated {
        detail: String,
    },
    Snapshot,
    SecurityEvent {
        detail: String,
    },
    /// BR-03 (полевой тест 2026-08-14): ход свободного цикла с
    /// инструментом — аудит «что именно делал агент»: имя инструмента,
    /// маскированные аргументы и наблюдение (S5/I4), решение
    /// (исполнен/отклонён). Свёртка состояния игнорирует, как все
    /// не-`StepApplied`/`Instantiated`.
    AgentToolTurn {
        step_id: String,
        tool: String,
        args_masked: String,
        observation_masked: String,
        ok: bool,
    },
    /// BR-03: программа codeact отклонена — маскированный текст и
    /// причина по стадии (`static_analysis`/`sandbox`). Диагностика
    /// отказов без внешнего прокси (полевой тест: BR-02 потребовал
    /// прокси только потому, что этого события не было).
    CodeActProgramRejected {
        step_id: String,
        attempt: u32,
        stage: String,
        reason: String,
        program_masked: String,
    },
    /// P8 (ADR-0012): инстанс явно переведён на новую версию графа
    /// операцией `migrate_version` — аудит-след «кто, когда, с какой
    /// версии на какую», не патч состояния (свёртка это событие
    /// игнорирует, как и все остальные не-`StepApplied`/`Instantiated`).
    VersionMigrated {
        from_version: u32,
        to_version: u32,
    },
    /// D5 (`deployment.md` §4): изменение доверенного списка репозиториев —
    /// событие, не сетевой эффект (I2), всегда после подтверждения
    /// человеком (`berimor trust add/remove`) или как побочный эффект
    /// успешной установки плагина из нового репозитория (D6). Журналуется
    /// под синтетическим `ProcessInstanceId("trust-list")`, отдельным от
    /// process instance любого реального процесса — доверенный список не
    /// принадлежит ни одному запуску. `state::fold` (Process Engine) это
    /// событие игнорирует, как и все остальные не-`StepApplied`/
    /// `Instantiated` — своя свёртка в `berimor_capability::trust_list`.
    /// `event.seq`/`event.ts_ms` уже дают `event_id`/`added_at` из
    /// формата записи `deployment.md` §4 — не дублируются здесь.
    TrustListChanged {
        action: TrustListAction,
        repo: String,
        allowed_ref: String,
        signer_identity: String,
        capability_ceiling: Vec<String>,
    },
    /// §20.22 v2: реестр живых сессий хоста (design-swarm-sessions.md,
    /// шаг 1). Пишется под синтетическим `ProcessInstanceId("host-sessions")`,
    /// как `TrustListChanged`; свёртка — в `berimor-cli::sessions`
    /// (последнее состояние на session_id). `command` — чем запущена
    /// сессия: "chat" | "run" | "daemon".
    SessionOpened {
        session_id: String,
        pid: u32,
        cwd: String,
        command: String,
    },
    /// Heartbeat сессии: в REPL — на границе хода, в демоне — на тике.
    /// Таймерный поток осознанно НЕ вводится (детерминизм).
    SessionHeartbeat {
        session_id: String,
    },
    /// Корректное завершение (best-effort: kill -9 события не будет —
    /// «мёртвая» вычисляется читателем по pid/порогу, не сторожком).
    SessionClosed {
        session_id: String,
    },
    /// §20.22 v2 шаг 2: сессия ИЗМЕНИЛА файл (files.write и родственные).
    /// Тот же синтетический инстанс host-sessions. `op` — имя инструмента.
    FileTouched {
        session_id: String,
        path: String,
        op: String,
    },
    /// Сессия ПРОЧИТАЛА файл (наблюдает за ним). Дедупликация «один путь
    /// одна запись за сессию» — на писателе (HashSet в диспетчере),
    /// журнал не раздувается.
    FileObserved {
        session_id: String,
        path: String,
    },
}

/// Действие над записью доверенного списка — см. [`EventKind::TrustListChanged`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustListAction {
    Added,
    Removed,
}

/// Материализованный кэш свёртки на момент `seq`. Ускоряет восстановление,
/// но не является источником истины — им остаётся журнал (`process-engine.md` §4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub process_instance: ProcessInstanceId,
    pub seq: EventSeq,
    pub state: serde_json::Value,
}
