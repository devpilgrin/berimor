//! Цикл исполнения движка: instantiate → (next → execute → apply → emit)*
//! → finish, снапшот при мутации.
//!
//! Источник: `docs/arch/process-engine.md` §4. ROADMAP: P3.
//!
//! `context_engine.build` (Context Engine, C1–C3) пропущен — состояние
//! передаётся исполнителю как есть; Mediation (M1–M7) и конкретные
//! Executors (E1–E9) ещё не реализованы, поэтому единственная точка,
//! куда движок передаёт исполнение — [`StepExecutor`]. Реальная
//! реализация этого трейта (когда появятся M1–M7/E1–E9) внутри себя
//! вызовет Executor::run, затем Mediation::commit; движок этого не видит
//! и не обязан видеть — это и есть разделение ответственности
//! (`mediation.md` §1).

use crate::{graph, state};
use berimor_storage::{EventLog, StorageError};
use berimor_types::{
    event::{Event, EventKind, EventSeq, ProcessInstanceId, Snapshot},
    step::{Patch, Process, Step, StepKind},
};
use serde_json::Value;
use std::time::{Duration, Instant};

/// Инстанс процесса — состояние + указатель на последний посещённый шаг +
/// версия графа, зафиксированная при создании на весь жизненный цикл
/// (ADR-0012).
#[derive(Debug, Clone)]
pub struct ProcessInstance {
    pub id: ProcessInstanceId,
    pub process: Process,
    pub state: Value,
    /// `None` — инстанс ещё не сделал ни шага (сразу после `instantiate`).
    pub current_step: Option<String>,
    /// Техдолг TD1.4 (`docs/audit-2026-07-31.md`): раньше `current_step`
    /// после `resume_after_human_gate_timeout` (политики `Fail`/
    /// `Escalate`) оставался как есть, а `run()` не проверял ничего —
    /// повторный вызов молча проходил мимо гейта, будто человек ответил.
    /// `Some(reason)` — инстанс заблокирован, `run()` отказывает СРАЗУ.
    /// Честно принятый остаточный пробел: публичного API «разблокировать»
    /// сегодня нет (см. doc-комментарий `resume_after_human_gate_timeout`)
    /// — блокировка постоянна до следующего `recover()` с ЖУРНАЛОМ, в
    /// котором это событие перестало быть последним (то есть до нового,
    /// пока не спроектированного механизма разрешения эскалации).
    pub blocked: Option<String>,
}

/// Единственная точка, где движок передаёт исполнение наружу — только для
/// шагов, которым это нужно (`tool`/`llm_structured`/`codeact`/`agent_step`).
/// Control-flow-типы (`sequential`/`branch`/`checkpoint`/`human_gate`)
/// движок обрабатывает сам, `execute` для них не вызывается.
pub trait StepExecutor {
    fn execute(&self, step: &Step, state: &Value) -> Result<Patch, ExecutorError>;
}

#[derive(Debug, thiserror::Error)]
#[error("исполнитель шага '{step_id}' завершился ошибкой: {reason}")]
pub struct ExecutorError {
    pub step_id: String,
    pub reason: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum RunOutcome {
    Finished,
    /// Цикл остановлен на `human_gate`; `current_step` инстанса указывает
    /// на этот шаг — повторный вызов `run` с той же реализацией
    /// [`EventLog`] продолжит с него же (`process-engine.md` §5:
    /// «ответ возобновляет выполнение»).
    AwaitingHuman {
        step_id: String,
        reason: String,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("превышен лимит процесса: {0}")]
    LimitExceeded(String),
    #[error("несовместимая версия процесса: инстанс на {instance}, передан граф версии {given}")]
    VersionMismatch { instance: u32, given: u32 },
    #[error("нарушение графа процесса: {0}")]
    Graph(#[from] graph::GraphError),
    #[error("ошибка хранилища: {0}")]
    Storage(#[from] StorageError),
    #[error(transparent)]
    Executor(#[from] ExecutorError),
    #[error("не поддержано: {0}")]
    Unsupported(String),
    /// Аудит 1.12: инстанс собран вручную минуя `compile` — внутренние
    /// инварианты («шаг существует в графе», «current_step задан») не
    /// выполнены. Раньше — паника `expect`; теперь честная ошибка.
    /// Полная приватизация полей — при мажорной версии API.
    #[error("инстанс собран в обход compile (инвариант нарушен): {detail}")]
    InvalidInstance { detail: String },
    /// P7: политика `on_timeout` шага — `Fail` (`process-engine.md` §5:
    /// «падение шага»).
    #[error("human_gate '{step_id}' не дождался ответа человека вовремя")]
    HumanGateTimeout { step_id: String },
    #[error("шаг '{0}' не human_gate — таймаут human_gate к нему неприменим")]
    NotAHumanGate(String),
    /// P8 (ADR-0012): текущий шаг инстанса не существует в графе новой
    /// версии — продолжать выполнение было бы некуда.
    #[error("миграция несовместима: шаг '{step_id}', на котором остановлен инстанс, отсутствует в новой версии графа")]
    MigrationIncompatible { step_id: String },
    /// Техдолг TD1.1: исполнитель вернул `Patch` с `step_id`, отличным от
    /// шага графа, для которого он был вызван — журналирование
    /// (`step_id` графа) и применение (`patch.step_id`) разошлись бы
    /// безвозвратно, если бы это не было отклонено здесь. Разведка
    /// подтвердила: ни один существующий исполнитель не производит
    /// расхождение легитимно — это ужесточение инварианта, не смена
    /// поведения для штатного пути.
    #[error("исполнитель шага '{expected}' вернул patch с чужим step_id '{actual}' — отклонено, не применено")]
    PatchStepIdMismatch { expected: String, actual: String },
    /// Техдолг TD1.2: `state.parallel.*` — служебный неймспейс барьера
    /// (`graph::parallel_branch_done`), доверенный движком БЕЗ разбора
    /// происхождения ключа. Входной JSON инстанса, содержащий верхнеуровневый
    /// ключ `parallel`, мог бы подделать барьер (пометить ветви
    /// «выполненными» до первого реального прогона) — явный отказ при
    /// `instantiate`, не молчаливая фильтрация (тот же принцип, что у
    /// `config::load` — невалидный вход есть ошибка, не тихий дефолт).
    #[error("вход процесса содержит зарезервированный служебный ключ верхнего уровня '{0}'")]
    ReservedStateKey(String),
    /// Находка 1.7 аудита: повторный instantiate с тем же id пересидировал
    /// бы состояние последним входом.
    #[error("инстанс '{0}' уже существует (журнал не пуст) — instantiate повторно недопустим")]
    InstanceExists(String),
    /// Находка 1.8 аудита: recover несуществующего давал пустой «призрак»
    /// без Instantiated в журнале.
    #[error("инстанс '{0}' не найден в журнале")]
    InstanceNotFound(String),
    /// Техдолг TD1.4: инстанс заблокирован (`ProcessInstance.blocked`,
    /// см. doc-комментарий поля) — обычно после `resume_after_human_gate_timeout`
    /// с политикой `Fail`/`Escalate`, никогда не пройдя легитимного
    /// подтверждения. `run()` отказывает СРАЗУ, до `next_step`.
    #[error("инстанс заблокирован ('{step_id}'): {reason}")]
    InstanceBlocked { step_id: String, reason: String },
}

/// Создаёт новый инстанс и пишет `Instantiated` первым событием журнала —
/// без этого `recover()` не может восстановить `input` после сбоя, он
/// иначе нигде не журналируется (найдено на этой же задаче, тестом
/// восстановления). `id` — ответственность вызывающего кода: стратегия
/// генерации идентификаторов не специфицирована ни в одном документе, не
/// выдумывается здесь.
pub fn instantiate(
    storage: &dyn EventLog,
    id: ProcessInstanceId,
    process: Process,
    input: Value,
) -> Result<ProcessInstance, EngineError> {
    graph::compile(&process)?;
    // TD1.2: `parallel` — служебный неймспейс барьера parallel-шагов
    // (`state.parallel.<fork>.<branch>`), не пользовательское поле
    // состояния. Проверка ДО записи `Instantiated` — единственная точка,
    // где это событие вообще журналируется (проверено, других
    // вызывающих мест нет), значит `fold` на воспроизведении журнала
    // гарантированно не встретит враждебный ключ ни в одном валидном
    // журнале.
    if let Value::Object(map) = &input {
        if map.contains_key("parallel") {
            return Err(EngineError::ReservedStateKey("parallel".to_string()));
        }
    }
    // Находка 1.7 аудита: повторный instantiate с тем же id дописывал
    // второй Instantiated — fold пересидировал состояние ПОСЛЕДНИМ
    // входом (первый терялся молча). Идентификатор инстанса — как
    // первичный ключ: занятый — ошибка, а не догадка о намерениях.
    if !storage.replay(&id)?.is_empty() {
        return Err(EngineError::InstanceExists(id.0.clone()));
    }
    storage.append(Event::new(
        id.clone(),
        process.version,
        EventKind::Instantiated,
        input.clone(),
    ))?;
    Ok(ProcessInstance {
        id,
        process,
        state: input,
        current_step: None,
        blocked: None,
    })
}

/// Восстанавливает инстанс сверткой журнала (I7): `state = fold(events)`,
/// `current_step` — id шага последнего события `StepApplied`. `process` —
/// граф, который восстановлению нужно передать явно: журнал хранит только
/// номер версии (`process_version`), не сам граф — реестр процессных
/// версий (ADR-0012) не входит в область P3.
///
/// Известное необязывающее ограничение (найдено независимым ревью P5,
/// не регрессия — то же свойство было верно и до P5 для human_gate):
/// `ParallelStepApplied` здесь не учитывается, только `StepApplied`. Если
/// сбой случился ПОСРЕДИ форка, `current_step` восстановится на шаг ДО
/// forка, не на сам fork — следующий `run()` сделает один лишний
/// no-op-переход, чтобы снова дойти до parallel-шага (тратит одну
/// единицу `max_steps`, но не искажает финальное состояние: барьер
/// решается по `state`, не по этому полю). См. также заведённую отдельно
/// задачу про `HumanGateOpened` без `step_id` — тот же класс пробела.
pub fn recover(
    storage: &dyn EventLog,
    process: Process,
    id: ProcessInstanceId,
) -> Result<ProcessInstance, EngineError> {
    graph::compile(&process)?;
    let events = storage.replay(&id)?;

    // Находка 1.8 аудита: recover несуществующего инстанса возвращал
    // Ok с пустым «призраком» — первый StepApplied попадал в журнал
    // без Instantiated (ровно тот дефект, ради исправления которого
    // Instantiated вводился). Пустой журнал = инстанса нет.
    if events.is_empty() {
        return Err(EngineError::InstanceNotFound(id.0.clone()));
    }
    if let Some(last) = events.last() {
        if last.process_version != process.version {
            return Err(EngineError::VersionMismatch {
                instance: last.process_version,
                given: process.version,
            });
        }
    }

    let state = state::fold(&events);
    let current_step = events.iter().rev().find_map(|event| match &event.kind {
        EventKind::StepApplied { step_id } => Some(step_id.clone()),
        _ => None,
    });
    // TD1.4: блокировка — свойство in-memory `ProcessInstance`, но обязана
    // переживать `recover()` в НОВОМ процессе (`--resume`), иначе
    // `resume_after_human_gate_timeout` защищала бы только текущий
    // процесс, не журнал. `HumanGateTimedOut` — единственное событие,
    // которое её выставляет (`resume_after_human_gate_timeout` ниже);
    // ничего не «снимает» блокировку в журнале сегодня (честный, не
    // решаемый здесь пробел — см. doc-комментарий `ProcessInstance::blocked`),
    // поэтому достаточно посмотреть на ПОСЛЕДНЕЕ событие целиком: если
    // это `HumanGateTimedOut` с политикой, которая не продолжает
    // исполнение сама (`branch` перезапускает выполнение немедленно,
    // здесь не сохраняется как "последнее" событие вообще), инстанс
    // заблокирован.
    let blocked = match events.last().map(|event| &event.kind) {
        Some(EventKind::HumanGateTimedOut { policy }) if policy != "branch" => {
            Some(format!("human_gate timeout, политика '{policy}'"))
        }
        _ => None,
    };

    Ok(ProcessInstance {
        id,
        process,
        state,
        current_step,
        blocked,
    })
}

/// Переводит работающий инстанс на новую версию графа (P8, ADR-0012).
///
/// Подтверждение человека (I2: «любое изменение системы только через
/// событие и подтверждение») — ответственность ВЫЗЫВАЮЩЕГО кода, не этой
/// функции: тот же приём, что и у `resume_after_human_gate_timeout`
/// («время вышло» — решение снаружи, функция применяет последствие).
/// Вызов `migrate_version` уже означает «подтверждение получено» — перед
/// вызовом стоит `ConfirmationHandler`/аналог, эта функция сама
/// диалогов не ведёт (I5: ядро не владеет UI).
///
/// «Проверка текущего состояния против схемы новой версии» —
/// `graph::compile` нового графа (сам по себе обязан быть валиден) плюс
/// проверка, что шаг, на котором остановлен инстанс, существует и в
/// новом графе: продолжать выполнение с несуществующего шага — не
/// восстановление, а порча состояния. Более глубокая совместимость схем
/// состояния (типы полей, обязательность) — вне этой проверки: Process
/// Engine не хранит схему состояния отдельно от шагов, которые его
/// пишут (`process-engine.md` §3).
pub fn migrate_version(
    storage: &dyn EventLog,
    instance: &mut ProcessInstance,
    new_process: Process,
) -> Result<(), EngineError> {
    graph::compile(&new_process)?;

    if let Some(step_id) = &instance.current_step {
        if !new_process.steps.iter().any(|s| &s.id == step_id) {
            return Err(EngineError::MigrationIncompatible {
                step_id: step_id.clone(),
            });
        }
    }

    let from_version = instance.process.version;
    let to_version = new_process.version;
    storage.append(Event::new(
        instance.id.clone(),
        to_version,
        EventKind::VersionMigrated {
            from_version,
            to_version,
        },
        Value::Null,
    ))?;

    instance.process = new_process;
    Ok(())
}

/// Прогоняет цикл `next → execute → apply → emit → snapshot` до
/// завершения, до `human_gate` или до превышения `max_steps`/`timeout`
/// (ROADMAP P6). Оба измеряются от начала ЭТОГО вызова `run`, не от
/// `instantiate` — тот же охват, что уже был у `steps_this_run` до этой
/// задачи: движок синхронный, без часов в состоянии (`process-engine.md`
/// §3), а `Instant` здесь эфемерен для одного вызова, не персистентное
/// поле инстанса — не противоречие, а то же решение, что и у P7
/// (`resume_after_human_gate_timeout`): отслеживание реального времени
/// ПОПЕРЁК перезапусков — забота вызывающего кода, не движка.
///
/// `token_budget`/`cost_budget` — заблокированы отсутствующей
/// отчётностью об использовании: `StepExecutor::execute` не возвращает
/// ни токены, ни стоимость (только `Patch`), а провайдеры Model Pool
/// (E3/E5) их не считают — принудить эти два прерывателя здесь означало
/// бы выдумать источник данных, которого в системе ещё нет, не входит в
/// эту задачу. `latency_budget_ms` — не прерыватель ЭТОГО цикла, а SLA
/// отбора провайдера на каждом шаге (ADR-0011); проброс из
/// `ProcessLimits` в `StepExecutor` — дело конкретной реализации трейта
/// (`berimor-cli::CliExecutor`), не движка.
pub fn run(
    storage: &dyn EventLog,
    executor: &dyn StepExecutor,
    instance: &mut ProcessInstance,
) -> Result<RunOutcome, EngineError> {
    // TD1.4 — проверяется ПЕРВЫМ, до чтения лимитов/графа: заблокированный
    // инстанс не должен потратить даже одну итерацию цикла.
    if let Some(reason) = &instance.blocked {
        return Err(EngineError::InstanceBlocked {
            step_id: instance.current_step.clone().unwrap_or_default(),
            reason: reason.clone(),
        });
    }

    let mut steps_this_run: u32 = 0;
    let started_at = Instant::now();
    let timeout = Duration::from_secs(instance.process.limits.timeout_seconds);

    loop {
        if steps_this_run >= instance.process.limits.max_steps {
            return Err(EngineError::LimitExceeded(format!(
                "max_steps = {}",
                instance.process.limits.max_steps
            )));
        }
        if started_at.elapsed() >= timeout {
            return Err(EngineError::LimitExceeded(format!(
                "timeout = {}s",
                instance.process.limits.timeout_seconds
            )));
        }
        steps_this_run += 1;

        let next = graph::next_step(
            &instance.process,
            instance.current_step.as_deref(),
            &instance.state,
        )?;
        let step_id = match next {
            graph::NextStep::Finished => return Ok(RunOutcome::Finished),
            graph::NextStep::Fork(remaining_branches) => {
                // `graph::next_step` уже отфильтровал выполненные ветви —
                // здесь просто исполняем ОДНУ из оставшихся за итерацию
                // («один писатель на инстанс», §4: цикл синхронный, не
                // параллельные потоки). `current_step` НЕ трогаем — он
                // остаётся на самом parallel-шаге, следующая итерация
                // снова спросит `next_step`: ещё есть ветви — форкнет
                // дальше, барьер пройден — пройдёт мимо как обычный шаг.
                let fork_step_id =
                    instance
                        .current_step
                        .clone()
                        .ok_or_else(|| EngineError::InvalidInstance {
                            detail: "current_step = None при ветвлении parallel".to_string(),
                        })?;
                let branch_step_id = remaining_branches
                    .first()
                    .ok_or_else(|| EngineError::InvalidInstance {
                        detail: "next_step вернул Fork с пустым списком ветвей".to_string(),
                    })?
                    .clone();
                execute_fork_branch(storage, executor, instance, &fork_step_id, &branch_step_id)?;
                continue;
            }
            graph::NextStep::Single(id) => id,
        };

        if let Some(outcome) = execute_single_step(storage, executor, instance, &step_id)? {
            return Ok(outcome);
        }
    }
}

/// Исполняет ОДИН шаг по его `StepKind` — общая логика между основным
/// циклом [`run`] и веткой `Branch` [`resume_after_human_gate_timeout`]
/// (техдолг TD1.3, `docs/audit-2026-07-31.md`): раньше та ветка просто
/// переставляла `current_step`, из-за чего `next_step` трактовал целевой
/// шаг как «уже посещённый» и следующий `run()` исполнял шаг ПОСЛЕ него,
/// не сам целевой. Теперь целевой шаг реально исполняется здесь же.
///
/// `Ok(Some(outcome))` — цикл обязан остановиться (`human_gate`);
/// `Ok(None)` — шаг исполнен, `current_step` обновлён, можно продолжать.
fn execute_single_step(
    storage: &dyn EventLog,
    executor: &dyn StepExecutor,
    instance: &mut ProcessInstance,
    step_id: &str,
) -> Result<Option<RunOutcome>, EngineError> {
    let step = instance
        .process
        .steps
        .iter()
        .find(|s| s.id == step_id)
        .ok_or_else(|| EngineError::InvalidInstance {
            detail: format!("шаг '{step_id}' отсутствует в графе инстанса"),
        })?;

    match &step.kind {
        StepKind::Sequential | StepKind::Branch { .. } | StepKind::Parallel { .. } => {
            // Ни один не мутирует состояние и не вызывает исполнителя
            // здесь — сюда попадаем только чтобы сдвинуть current_step
            // на сам parallel-шаг; следующая итерация `graph::next_step`
            // либо разрешит branch, либо начнёт форк (реальное
            // исполнение ветвей — ветка `Fork` в `run()`, не эта).
        }
        StepKind::Checkpoint => {
            let seq = latest_seq(storage, &instance.id)?;
            storage.write_snapshot(Snapshot {
                process_instance: instance.id.clone(),
                seq,
                state: instance.state.clone(),
            })?;
        }
        StepKind::HumanGate {
            reason_template, ..
        } => {
            let reason = reason_template.clone();
            instance.current_step = Some(step_id.to_string());
            return Ok(Some(RunOutcome::AwaitingHuman {
                step_id: step_id.to_string(),
                // Резолвинг {{state...}} в шаблоне — забота Context
                // Engine/Mediation (ещё не реализованы); отдаём как есть.
                reason,
            }));
        }
        StepKind::Loop { .. } => {
            return Err(EngineError::Unsupported(
                "loop: нет поля цели повтора — открытый вопрос архитектуры (см. graph.rs)".into(),
            ))
        }
        StepKind::Tool { .. }
        | StepKind::LlmStructured { .. }
        | StepKind::CodeAct { .. }
        | StepKind::AgentStep { .. } => {
            let patch = executor.execute(step, &instance.state)?;
            // Техдолг TD1.1: `patch.step_id` — поле, независимое от
            // `step_id` графа, которым журналируется событие ниже;
            // разведка подтвердила, что ни один существующий исполнитель
            // не производит расхождение легитимно. Без этой проверки
            // живое состояние (`apply_patch`, ключ — `patch.step_id`) и
            // свёртка журнала (`fold`, ключ — `step_id` из `StepApplied`)
            // расходились бы молча и безвозвратно.
            if patch.step_id != step_id {
                return Err(EngineError::PatchStepIdMismatch {
                    expected: step_id.to_string(),
                    actual: patch.step_id,
                });
            }
            let event = Event::new(
                instance.id.clone(),
                instance.process.version,
                EventKind::StepApplied {
                    step_id: step_id.to_string(),
                },
                patch.changes.clone(),
            );
            let seq = storage.append(event)?;
            instance.state = state::apply_patch(&instance.state, &patch);
            storage.write_snapshot(Snapshot {
                process_instance: instance.id.clone(),
                seq,
                state: instance.state.clone(),
            })?;
        }
    }

    instance.current_step = Some(step_id.to_string());
    Ok(None)
}

/// Исполняет ОДНУ ветвь parallel-шага (P5): вызывает исполнителя как для
/// обычного мутирующего шага, но журналирует `ParallelStepApplied`
/// (не `StepApplied`) и применяет патч в изолированный неймспейс
/// `state.parallel.<fork_step_id>.<branch_step_id>`
/// ([`state::apply_parallel_patch`]), не в `state.<branch_step_id>`
/// напрямую. Тип ветви гарантирован `graph::compile`
/// (`GraphError::ParallelBranchNotMutating`) — здесь не перепроверяется.
fn execute_fork_branch(
    storage: &dyn EventLog,
    executor: &dyn StepExecutor,
    instance: &mut ProcessInstance,
    fork_step_id: &str,
    branch_step_id: &str,
) -> Result<(), EngineError> {
    let branch_step = instance
        .process
        .steps
        .iter()
        .find(|s| s.id == branch_step_id)
        .ok_or_else(|| EngineError::InvalidInstance {
            detail: format!("ветвь '{branch_step_id}' отсутствует в графе инстанса"),
        })?;

    let patch = executor.execute(branch_step, &instance.state)?;
    let event = Event::new(
        instance.id.clone(),
        instance.process.version,
        EventKind::ParallelStepApplied {
            fork_step_id: fork_step_id.to_string(),
            branch_step_id: branch_step_id.to_string(),
        },
        patch.changes.clone(),
    );
    let seq = storage.append(event)?;
    instance.state = state::apply_parallel_patch(
        &instance.state,
        fork_step_id,
        branch_step_id,
        &patch.changes,
    );
    storage.write_snapshot(Snapshot {
        process_instance: instance.id.clone(),
        seq,
        state: instance.state.clone(),
    })?;
    Ok(())
}

fn latest_seq(
    storage: &dyn EventLog,
    instance: &ProcessInstanceId,
) -> Result<EventSeq, StorageError> {
    Ok(storage
        .replay(instance)?
        .last()
        .map(|e| e.seq)
        .unwrap_or(EventSeq(0)))
}

/// Применяет политику `on_timeout` шага `human_gate` (`process-engine.md`
/// §5, ROADMAP: P7) после того, как вызывающий код решил, что ответ
/// человека не пришёл вовремя.
///
/// Отслеживание прошедшего времени — не забота движка: он синхронный, без
/// собственных часов в состоянии (`process-engine.md` §3 не описывает
/// таймер как часть состояния процесса). Решение «время вышло» — задача
/// вызывающего кода (CLI, будущая интеграция с планировщиком A5, Фаза 7);
/// эта функция реализует только то, что происходит ДАЛЬШЕ, по декларации
/// политики.
///
/// `Escalate` не выполняет реальную маршрутизацию эскалации сама (I5:
/// ядро не имеет обязательных внешних зависимостей) — только журналирует
/// `EventKind::HumanGateTimedOut` и помечает инстанс заблокированным
/// (`ProcessInstance.blocked`, техдолг TD1.4 — раньше «инстанс остаётся
/// на паузе» было утверждением doc-комментария, не поведением: ничто не
/// мешало следующему `run()` пройти мимо гейта). Дальнейшая обработка
/// события — по-прежнему забота внешнего наблюдателя (диспетчер Actors,
/// Фаза 7, или человек напрямую); официального API «разблокировать»
/// сегодня нет — честно принятый остаточный пробел, не решаемый здесь.
///
/// `Fail` — та же блокировка, по той же причине: раньше `current_step`
/// тоже оставался нетронутым, и повторный `run()` после уже
/// произошедшего `HumanGateTimeout` проходил мимо точно так же, как
/// после `Escalate`.
///
/// `Branch { to }` — техдолг TD1.3: раньше просто переставлял
/// `current_step = Some(to)`, из-за чего `next_step` трактовал `to` как
/// «уже посещённый» и следующий `run()` исполнял шаг ПОСЛЕ `to`, не сам
/// `to`. Теперь `to` реально исполняется здесь через [`execute_single_step`]
/// (тот же код, что использует основной цикл `run()`) — отсюда и новый
/// параметр `executor`. Редирект на ДРУГОЙ `human_gate` — вырожденный
/// случай, явно отклонён (не пытается решить, что означает «AwaitingHuman
/// из функции, возвращающей `Result<(), _>`»), не входит в эту задачу.
pub fn resume_after_human_gate_timeout(
    storage: &dyn EventLog,
    executor: &dyn StepExecutor,
    instance: &mut ProcessInstance,
    step_id: &str,
) -> Result<(), EngineError> {
    let step = instance
        .process
        .steps
        .iter()
        .find(|s| s.id == step_id)
        .ok_or_else(|| graph::GraphError::UnknownStep(step_id.to_string()))?;
    let StepKind::HumanGate { on_timeout, .. } = &step.kind else {
        return Err(EngineError::NotAHumanGate(step_id.to_string()));
    };
    let on_timeout = on_timeout.clone();

    let policy_label = match &on_timeout {
        berimor_types::step::HumanGateTimeoutPolicy::Fail => "fail",
        berimor_types::step::HumanGateTimeoutPolicy::Branch { .. } => "branch",
        berimor_types::step::HumanGateTimeoutPolicy::Escalate => "escalate",
    };
    storage.append(Event::new(
        instance.id.clone(),
        instance.process.version,
        EventKind::HumanGateTimedOut {
            policy: policy_label.to_string(),
        },
        Value::Null,
    ))?;

    match on_timeout {
        berimor_types::step::HumanGateTimeoutPolicy::Fail => {
            instance.blocked = Some(format!("human_gate '{step_id}' — таймаут, политика fail"));
            Err(EngineError::HumanGateTimeout {
                step_id: step_id.to_string(),
            })
        }
        berimor_types::step::HumanGateTimeoutPolicy::Branch { to } => {
            let target = instance
                .process
                .steps
                .iter()
                .find(|s| s.id == to)
                .ok_or_else(|| graph::GraphError::UnknownStep(to.clone()))?;
            if matches!(target.kind, StepKind::HumanGate { .. }) {
                return Err(EngineError::Unsupported(format!(
                    "on_timeout: branch на другой human_gate ('{to}') не поддержан"
                )));
            }
            execute_single_step(storage, executor, instance, &to)?;
            Ok(())
        }
        berimor_types::step::HumanGateTimeoutPolicy::Escalate => {
            instance.blocked = Some(format!(
                "human_gate '{step_id}' — таймаут, политика escalate"
            ));
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse;
    use berimor_storage::SqliteEventLog;
    use serde_json::json;

    const GOLDEN_FIXTURE: &str =
        include_str!("../../../fixtures/golden/processes/card-delivery-support.yaml");

    /// Находка 1.7 аудита: повторный instantiate — ошибка, вход первого
    /// инстанса не пересидируется.
    #[test]
    fn instantiate_twice_with_same_id_is_rejected() {
        let storage = SqliteEventLog::open_in_memory().unwrap();
        let process = parse(GOLDEN_FIXTURE).unwrap();
        let id = ProcessInstanceId("i-1-7".into());
        instantiate(&storage, id.clone(), process.clone(), json!({"a": 1})).unwrap();
        let err = instantiate(&storage, id, process, json!({"a": 2})).unwrap_err();
        assert!(matches!(err, EngineError::InstanceExists(_)), "{err:?}");
    }

    /// Аудит 1.12 (шаг а): инстанс, собранный вручную минуя compile с
    /// веткой на несуществующий шаг, — честная ошибка, не паника expect.
    #[test]
    fn hand_built_instance_with_dangling_branch_errors_not_panics() {
        let storage = SqliteEventLog::open_in_memory().unwrap();
        let process = parse(
            r#"process: hand-built
version: 1
steps:
  - id: route
    type: branch
    on: state.flag
    cases:
      go: ghost-target
limits:
  max_steps: 10
  timeout: 5m
"#,
        )
        .unwrap();
        // Ручная сборка минуя compile: ветка ведёт на шаг, которого нет
        // в графе — compile бы это отклонил, поля публичны (1.12).
        let mut instance = ProcessInstance {
            id: ProcessInstanceId("i-1-12".into()),
            process,
            state: json!({"flag": "go"}),
            current_step: None,
            blocked: None,
        };
        let executor = FakeExecutor { risk: 1 };
        let err = run(&storage, &executor, &mut instance).unwrap_err();
        assert!(
            matches!(err, EngineError::InvalidInstance { .. }),
            "ожидалась InvalidInstance, не паника и не иная ошибка: {err:?}"
        );
    }

    /// Находка 1.8 аудита: recover несуществующего — ошибка, не призрак.
    #[test]
    fn recover_nonexistent_instance_is_error_not_ghost() {
        let storage = SqliteEventLog::open_in_memory().unwrap();
        let process = parse(GOLDEN_FIXTURE).unwrap();
        let err = recover(&storage, process, ProcessInstanceId("ghost".into())).unwrap_err();
        assert!(matches!(err, EngineError::InstanceNotFound(_)), "{err:?}");
    }

    /// Фейковый исполнитель — E1-E9/M1-M7 ещё не реализованы (см. doc-комментарий
    /// модуля). Возвращает заранее заданные патчи по id шага, чтобы прогнать
    /// цикл end-to-end без модели и без сети — ровно цель Milestone 0.
    struct FakeExecutor {
        risk: i64,
    }

    impl StepExecutor for FakeExecutor {
        fn execute(&self, step: &Step, _state: &Value) -> Result<Patch, ExecutorError> {
            let changes = match step.id.as_str() {
                "classify" => json!({"risk": self.risk, "category": "card"}),
                "fetch_card_status" => json!({"status": "active"}),
                "answer" => json!({"reply": "готово"}),
                other => {
                    return Err(ExecutorError {
                        step_id: other.into(),
                        reason: "FakeExecutor не знает этот шаг".into(),
                    })
                }
            };
            Ok(Patch {
                step_id: step.id.clone(),
                changes,
            })
        }
    }

    fn instance(id: &str, risk: i64) -> (SqliteEventLog, ProcessInstance, FakeExecutor) {
        let process = parse(GOLDEN_FIXTURE).unwrap();
        let storage = SqliteEventLog::open_in_memory().unwrap();
        let inst = instantiate(
            &storage,
            ProcessInstanceId(id.into()),
            process,
            json!({"user": {"card_id": "c-1"}}),
        )
        .unwrap();
        (storage, inst, FakeExecutor { risk })
    }

    #[test]
    fn low_risk_path_runs_to_completion_without_human_gate() {
        let (storage, mut inst, executor) = instance("low-risk", 2);

        let outcome = run(&storage, &executor, &mut inst).unwrap();

        assert_eq!(outcome, RunOutcome::Finished);
        assert_eq!(inst.current_step.as_deref(), Some("answer"));
        assert_eq!(
            inst.state,
            json!({
                "user": {"card_id": "c-1"},
                "classify": {"risk": 2, "category": "card"},
                "fetch_card_status": {"status": "active"},
                "answer": {"reply": "готово"},
            })
        );
    }

    #[test]
    fn high_risk_path_pauses_at_human_gate() {
        let (storage, mut inst, executor) = instance("high-risk", 8);

        let outcome = run(&storage, &executor, &mut inst).unwrap();

        assert_eq!(
            outcome,
            RunOutcome::AwaitingHuman {
                step_id: "human_review".into(),
                reason: "высокий риск: {{state.classify.risk}}".into(),
            }
        );
        assert_eq!(inst.current_step.as_deref(), Some("human_review"));
        // classify уже отработал и попал в состояние — human_gate его не отменяет
        assert_eq!(
            inst.state["classify"],
            json!({"risk": 8, "category": "card"})
        );
    }

    #[test]
    fn resuming_after_human_gate_continues_to_completion() {
        let (storage, mut inst, executor) = instance("resume", 8);

        let paused = run(&storage, &executor, &mut inst).unwrap();
        assert!(matches!(paused, RunOutcome::AwaitingHuman { .. }));

        // «человек подтвердил» — повторный run с тем же current_step
        // (process-engine.md §5: «ответ возобновляет выполнение»).
        let finished = run(&storage, &executor, &mut inst).unwrap();

        assert_eq!(finished, RunOutcome::Finished);
        assert_eq!(inst.current_step.as_deref(), Some("answer"));
        assert_eq!(inst.state["fetch_card_status"], json!({"status": "active"}));
    }

    #[test]
    fn events_are_recorded_only_for_mutating_steps() {
        let (storage, mut inst, executor) = instance("events", 2);
        run(&storage, &executor, &mut inst).unwrap();

        let events = storage.replay(&inst.id).unwrap();
        let step_ids: Vec<&str> = events
            .iter()
            .filter_map(|e| match &e.kind {
                EventKind::StepApplied { step_id } => Some(step_id.as_str()),
                _ => None,
            })
            .collect();

        // classify, check_risk (branch, не мутирует), fetch_card_status, answer
        // -> событий StepApplied должно быть три: classify, fetch_card_status, answer.
        // check_risk — branch, не производит патч, события не пишет.
        assert_eq!(step_ids, vec!["classify", "fetch_card_status", "answer"]);
    }

    /// Техдолг TD1.1 (`docs/audit-2026-07-31.md`): исполнитель, вернувший
    /// `Patch` с чужим `step_id`, раньше рассинхронизировал живое
    /// состояние (`apply_patch`, ключ — `patch.step_id`) и свёртку
    /// журнала (`fold`, ключ — `step_id` графа из `StepApplied`)
    /// безвозвратно и молча. Теперь — явный отказ, ни `StepApplied`, ни
    /// изменение состояния не должны произойти.
    struct ForgesStepIdExecutor;
    impl StepExecutor for ForgesStepIdExecutor {
        fn execute(&self, step: &Step, _state: &Value) -> Result<Patch, ExecutorError> {
            let _ = &step.id;
            Ok(Patch {
                step_id: "classify_FORGED".to_string(),
                changes: json!({"risk": 1}),
            })
        }
    }

    #[test]
    fn patch_with_a_forged_step_id_is_rejected_not_silently_applied() {
        let process = parse(GOLDEN_FIXTURE).unwrap();
        let storage = SqliteEventLog::open_in_memory().unwrap();
        let mut inst = instantiate(
            &storage,
            ProcessInstanceId("forged-step-id".into()),
            process,
            json!({"user": {"card_id": "c-1"}}),
        )
        .unwrap();

        let result = run(&storage, &ForgesStepIdExecutor, &mut inst);

        assert!(matches!(
            result,
            Err(EngineError::PatchStepIdMismatch { expected, actual })
                if expected == "classify" && actual == "classify_FORGED"
        ));
        assert!(inst.state.get("classify").is_none());
        assert!(inst.state.get("classify_FORGED").is_none());
        let events = storage.replay(&inst.id).unwrap();
        assert!(!events
            .iter()
            .any(|e| matches!(&e.kind, EventKind::StepApplied { .. })));
    }

    /// Техдолг TD1.2: `state.parallel.*` — служебный неймспейс барьера,
    /// доверенный движком без разбора происхождения. Враждебный вход,
    /// подделывающий этот ключ, обязан быть отклонён при `instantiate`,
    /// не тихо просочиться в состояние.
    #[test]
    fn instantiate_rejects_input_seeding_the_reserved_parallel_namespace() {
        let process = parse(GOLDEN_FIXTURE).unwrap();
        let storage = SqliteEventLog::open_in_memory().unwrap();

        let result = instantiate(
            &storage,
            ProcessInstanceId("hostile-parallel-key".into()),
            process,
            json!({"parallel": {"fanout": {"branch_a": 1, "branch_b": 2}}}),
        );

        assert!(matches!(
            result,
            Err(EngineError::ReservedStateKey(key)) if key == "parallel"
        ));
        // Ничего не должно было попасть в журнал — отказ до append().
        let events = storage
            .replay(&ProcessInstanceId("hostile-parallel-key".into()))
            .unwrap();
        assert!(events.is_empty());
    }

    /// Центральное свойство Milestone 0 (`docs/ROADMAP.md` §3): повторный
    /// `recover()` по журналу восстанавливает то же состояние, и
    /// восстановленный инстанс можно продолжить до того же результата,
    /// что и непрерывный прогон.
    #[test]
    fn recover_after_partial_run_reconstructs_same_state_and_can_resume() {
        let process = parse(GOLDEN_FIXTURE).unwrap();
        let storage = SqliteEventLog::open_in_memory().unwrap();
        let id = ProcessInstanceId("crash-recovery".into());
        let executor = FakeExecutor { risk: 2 };

        // "падение после classify": прогоняем один мутирующий шаг и
        // прекращаем работу с этим инстансом в памяти — как будто процесс
        // перезапустился, в памяти ничего не осталось, кроме журнала.
        {
            let mut inst = instantiate(
                &storage,
                id.clone(),
                process.clone(),
                json!({"user": {"card_id": "c-1"}}),
            )
            .unwrap();
            let outcome = run_n_steps(&storage, &executor, &mut inst, 1);
            assert_eq!(inst.current_step.as_deref(), Some("classify"));
            drop(outcome);
        }

        // Восстановление из журнала — новый ProcessInstance, без связи с предыдущим.
        let mut recovered = recover(&storage, process, id).unwrap();
        assert_eq!(recovered.current_step.as_deref(), Some("classify"));
        assert_eq!(
            recovered.state,
            json!({"user": {"card_id": "c-1"}, "classify": {"risk": 2, "category": "card"}})
        );

        // Продолжаем восстановленный инстанс до конца.
        let outcome = run(&storage, &executor, &mut recovered).unwrap();
        assert_eq!(outcome, RunOutcome::Finished);
        assert_eq!(
            recovered.state,
            json!({
                "user": {"card_id": "c-1"},
                "classify": {"risk": 2, "category": "card"},
                "fetch_card_status": {"status": "active"},
                "answer": {"reply": "готово"},
            })
        );
    }

    /// Прогоняет не более `n` "видимых" (мутирующих или control-flow) шагов
    /// вручную — вспомогательная функция только для теста восстановления,
    /// где нужно остановиться на середине прогона намеренно.
    fn run_n_steps(
        storage: &dyn EventLog,
        executor: &dyn StepExecutor,
        inst: &mut ProcessInstance,
        n: usize,
    ) -> RunOutcome {
        for _ in 0..n {
            let next =
                graph::next_step(&inst.process, inst.current_step.as_deref(), &inst.state).unwrap();
            let step_id = match next {
                graph::NextStep::Single(id) => id,
                graph::NextStep::Finished => return RunOutcome::Finished,
                graph::NextStep::Fork(_) => unreachable!("golden-фикстура не содержит parallel"),
            };
            let step = inst.process.steps.iter().find(|s| s.id == step_id).unwrap();
            if matches!(
                step.kind,
                StepKind::Tool { .. } | StepKind::LlmStructured { .. }
            ) {
                let patch = executor.execute(step, &inst.state).unwrap();
                let event = Event::new(
                    inst.id.clone(),
                    inst.process.version,
                    EventKind::StepApplied {
                        step_id: step_id.clone(),
                    },
                    patch.changes.clone(),
                );
                let seq = storage.append(event).unwrap();
                inst.state = state::apply_patch(&inst.state, &patch);
                storage
                    .write_snapshot(Snapshot {
                        process_instance: inst.id.clone(),
                        seq,
                        state: inst.state.clone(),
                    })
                    .unwrap();
            }
            inst.current_step = Some(step_id);
        }
        RunOutcome::AwaitingHuman {
            step_id: inst.current_step.clone().unwrap(),
            reason: String::new(),
        }
    }

    #[test]
    fn max_steps_limit_is_enforced() {
        let mut process = parse(GOLDEN_FIXTURE).unwrap();
        process.limits.max_steps = 1;
        let storage = SqliteEventLog::open_in_memory().unwrap();
        let mut inst = instantiate(
            &storage,
            ProcessInstanceId("limited".into()),
            process,
            json!({"user": {"card_id": "c-1"}}),
        )
        .unwrap();
        let executor = FakeExecutor { risk: 2 };

        let result = run(&storage, &executor, &mut inst);
        assert!(matches!(result, Err(EngineError::LimitExceeded(_))));
    }

    #[test]
    fn timeout_limit_is_enforced_before_any_step_executes() {
        // P6: `timeout: 0s` — уже истёк на первой же проверке, до первого
        // шага. `max_steps` щедрый, чтобы не маскировать именно эту проверку.
        let mut process = parse(GOLDEN_FIXTURE).unwrap();
        process.limits.timeout_seconds = 0;
        let storage = SqliteEventLog::open_in_memory().unwrap();
        let mut inst = instantiate(
            &storage,
            ProcessInstanceId("timed-out".into()),
            process,
            json!({"user": {"card_id": "c-1"}}),
        )
        .unwrap();
        let executor = FakeExecutor { risk: 2 };

        let result = run(&storage, &executor, &mut inst);

        assert!(matches!(result, Err(EngineError::LimitExceeded(_))));
        // Ни одного StepApplied — таймаут остановил цикл раньше исполнителя.
        let events = storage.replay(&inst.id).unwrap();
        assert!(
            !events
                .iter()
                .any(|e| matches!(e.kind, EventKind::StepApplied { .. })),
            "таймаут обязан сработать до вызова исполнителя"
        );
    }

    #[test]
    fn generous_timeout_does_not_interfere_with_a_normal_run() {
        // Тот же приём, что и с max_steps: щедрый лимит не должен ничего
        // менять в обычном прогоне.
        let (storage, mut inst, executor) = instance("generous-timeout", 2);

        let outcome = run(&storage, &executor, &mut inst).unwrap();

        assert_eq!(outcome, RunOutcome::Finished);
    }

    #[test]
    fn recover_rejects_mismatched_process_version() {
        let process = parse(GOLDEN_FIXTURE).unwrap();
        let storage = SqliteEventLog::open_in_memory().unwrap();
        let id = ProcessInstanceId("version-mismatch".into());
        let executor = FakeExecutor { risk: 2 };

        {
            let mut inst = instantiate(
                &storage,
                id.clone(),
                process.clone(),
                json!({"user": {"card_id": "c-1"}}),
            )
            .unwrap();
            run_n_steps(&storage, &executor, &mut inst, 1);
        }

        let mut newer_process = process;
        newer_process.version += 1;

        let result = recover(&storage, newer_process, id);
        assert!(matches!(result, Err(EngineError::VersionMismatch { .. })));
    }

    #[test]
    fn executor_failure_does_not_write_a_partial_event() {
        struct AlwaysFails;
        impl StepExecutor for AlwaysFails {
            fn execute(&self, step: &Step, _state: &Value) -> Result<Patch, ExecutorError> {
                Err(ExecutorError {
                    step_id: step.id.clone(),
                    reason: "намеренный сбой теста".into(),
                })
            }
        }

        let process = parse(GOLDEN_FIXTURE).unwrap();
        let storage = SqliteEventLog::open_in_memory().unwrap();
        let id = ProcessInstanceId("executor-failure".into());
        let mut inst = instantiate(
            &storage,
            id.clone(),
            process,
            json!({"user": {"card_id": "c-1"}}),
        )
        .unwrap();

        let result = run(&storage, &AlwaysFails, &mut inst);

        assert!(matches!(result, Err(EngineError::Executor(_))));
        let events = storage.replay(&id).unwrap();
        assert!(
            !events
                .iter()
                .any(|e| matches!(e.kind, EventKind::StepApplied { .. })),
            "неудачный вызов исполнителя не должен оставлять StepApplied в журнале \
             (Instantiated от самого instantiate() — не в счёт, это отдельное событие)"
        );
    }

    // --- P5: parallel/join-барьер -----------------------------------------

    const FANOUT_YAML: &str = "
process: p
version: 1
limits:
  max_steps: 10
  timeout: 10m
steps:
  - id: fanout
    type: parallel
    branches: [branch_a, branch_b]
  - id: branch_a
    type: tool
    tool: some.tool
    args: {}
  - id: branch_b
    type: tool
    tool: some.tool
    args: {}
  - id: after
    type: tool
    tool: some.tool
    args: {}
";

    struct FanoutExecutor {
        /// Если задано — исполнитель падает на этом шаге ровно один раз
        /// (для проверки восстановления посреди форка).
        fail_once_on: std::cell::RefCell<Option<&'static str>>,
    }

    impl FanoutExecutor {
        fn new() -> Self {
            Self {
                fail_once_on: std::cell::RefCell::new(None),
            }
        }

        fn failing_once_on(step_id: &'static str) -> Self {
            Self {
                fail_once_on: std::cell::RefCell::new(Some(step_id)),
            }
        }
    }

    impl StepExecutor for FanoutExecutor {
        fn execute(&self, step: &Step, _state: &Value) -> Result<Patch, ExecutorError> {
            if self.fail_once_on.borrow().as_deref() == Some(step.id.as_str()) {
                *self.fail_once_on.borrow_mut() = None;
                return Err(ExecutorError {
                    step_id: step.id.clone(),
                    reason: "намеренный сбой теста".into(),
                });
            }
            let changes = match step.id.as_str() {
                "branch_a" => json!({"result": "a"}),
                "branch_b" => json!({"result": "b"}),
                "after" => json!({"done": true}),
                other => {
                    return Err(ExecutorError {
                        step_id: other.into(),
                        reason: "FanoutExecutor не знает этот шаг".into(),
                    })
                }
            };
            Ok(Patch {
                step_id: step.id.clone(),
                changes,
            })
        }
    }

    #[test]
    fn parallel_step_runs_to_completion_through_the_barrier() {
        let (storage, mut inst) = instance_with_process(FANOUT_YAML, "fanout-run");
        let executor = FanoutExecutor::new();

        let outcome = run(&storage, &executor, &mut inst).unwrap();

        assert_eq!(outcome, RunOutcome::Finished);
        assert_eq!(
            inst.state,
            json!({
                "parallel": {"fanout": {
                    "branch_a": {"result": "a"},
                    "branch_b": {"result": "b"}
                }},
                "after": {"done": true}
            })
        );
    }

    #[test]
    fn parallel_branches_are_journaled_under_their_own_namespace_not_top_level() {
        let (storage, mut inst) = instance_with_process(FANOUT_YAML, "fanout-namespace");
        let executor = FanoutExecutor::new();

        run(&storage, &executor, &mut inst).unwrap();

        assert_eq!(
            inst.state["parallel"]["fanout"]["branch_a"],
            json!({"result": "a"})
        );
        assert_eq!(
            inst.state["parallel"]["fanout"]["branch_b"],
            json!({"result": "b"})
        );
        // Ветви НЕ появляются как обычные top-level шаги.
        assert!(inst.state.get("branch_a").is_none());
        assert!(inst.state.get("branch_b").is_none());
        assert_eq!(inst.state["after"], json!({"done": true}));
    }

    #[test]
    fn parallel_branches_journal_parallel_step_applied_not_step_applied() {
        let (storage, mut inst) = instance_with_process(FANOUT_YAML, "fanout-events");
        let executor = FanoutExecutor::new();

        run(&storage, &executor, &mut inst).unwrap();

        let events = storage.replay(&inst.id).unwrap();
        let parallel_events: Vec<&str> = events
            .iter()
            .filter_map(|e| match &e.kind {
                EventKind::ParallelStepApplied { branch_step_id, .. } => {
                    Some(branch_step_id.as_str())
                }
                _ => None,
            })
            .collect();
        assert_eq!(parallel_events, vec!["branch_a", "branch_b"]);

        let step_applied: Vec<&str> = events
            .iter()
            .filter_map(|e| match &e.kind {
                EventKind::StepApplied { step_id } => Some(step_id.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            step_applied,
            vec!["after"],
            "ветви форка не должны порождать обычный StepApplied"
        );
    }

    #[test]
    fn each_fork_branch_counts_toward_max_steps() {
        let mut process = parse(FANOUT_YAML).unwrap();
        // Шаг 1 — переход на сам "fanout" (Single, без исполнения), шаг 2 —
        // первая ветвь; бюджета на вторую ветвь уже не хватает.
        process.limits.max_steps = 2;
        let storage = SqliteEventLog::open_in_memory().unwrap();
        let mut inst = instantiate(
            &storage,
            ProcessInstanceId("fanout-max-steps".into()),
            process,
            json!({}),
        )
        .unwrap();
        let executor = FanoutExecutor::new();

        let result = run(&storage, &executor, &mut inst);

        assert!(matches!(result, Err(EngineError::LimitExceeded(_))));
        // Первая ветвь успела примениться до того, как лимит остановил цикл.
        assert_eq!(
            inst.state["parallel"]["fanout"]["branch_a"],
            json!({"result": "a"})
        );
        assert!(inst.state["parallel"]["fanout"].get("branch_b").is_none());
    }

    #[test]
    fn failed_branch_does_not_advance_and_can_be_retried() {
        let (storage, mut inst) = instance_with_process(FANOUT_YAML, "fanout-retry");
        let executor = FanoutExecutor::failing_once_on("branch_b");

        let first = run(&storage, &executor, &mut inst);
        assert!(
            first.is_err(),
            "branch_b намеренно падает на первой попытке"
        );
        assert_eq!(
            inst.state["parallel"]["fanout"]["branch_a"],
            json!({"result": "a"}),
            "branch_a уже применилась до отказа branch_b"
        );
        assert!(inst.state["parallel"]["fanout"].get("branch_b").is_none());

        // Повторный run() с тем же (уже не падающим) исполнителем
        // доисполняет только оставшуюся ветвь, не переисполняет branch_a.
        let second = run(&storage, &executor, &mut inst).unwrap();
        assert_eq!(second, RunOutcome::Finished);
        assert_eq!(
            inst.state["parallel"]["fanout"]["branch_b"],
            json!({"result": "b"})
        );

        let events = storage.replay(&inst.id).unwrap();
        let branch_a_events = events
            .iter()
            .filter(
                |e| matches!(&e.kind, EventKind::ParallelStepApplied { branch_step_id, .. } if branch_step_id == "branch_a"),
            )
            .count();
        assert_eq!(
            branch_a_events, 1,
            "branch_a не должна исполниться повторно"
        );
    }

    #[test]
    fn recover_mid_fork_resumes_only_the_remaining_branch() {
        let process = parse(FANOUT_YAML).unwrap();
        let storage = SqliteEventLog::open_in_memory().unwrap();
        let id = ProcessInstanceId("fanout-recover".into());

        {
            let mut inst = instantiate(&storage, id.clone(), process.clone(), json!({})).unwrap();
            let executor = FanoutExecutor::failing_once_on("branch_b");
            let _ = run(&storage, &executor, &mut inst); // падает на branch_b, branch_a уже в журнале
        }

        // "перезапуск процесса" — восстановление из журнала, без связи с
        // предыдущим инстансом (тот же приём, что и P4).
        let mut recovered = recover(&storage, process, id).unwrap();
        assert_eq!(
            recovered.state["parallel"]["fanout"]["branch_a"],
            json!({"result": "a"})
        );
        assert!(recovered.state["parallel"]["fanout"]
            .get("branch_b")
            .is_none());

        let executor = FanoutExecutor::new();
        let outcome = run(&storage, &executor, &mut recovered).unwrap();

        assert_eq!(outcome, RunOutcome::Finished);
        assert_eq!(
            recovered.state["parallel"]["fanout"]["branch_b"],
            json!({"result": "b"})
        );
        assert_eq!(recovered.state["after"], json!({"done": true}));
    }

    // --- P8: migrate_version --------------------------------------------

    const UNRELATED_PROCESS_YAML: &str = "
process: p
version: 99
limits:
  max_steps: 10
  timeout: 10m
steps:
  - id: unrelated_step
    type: sequential
";

    #[test]
    fn migrate_version_updates_the_instance_process_and_journals_the_event() {
        let (storage, mut inst, executor) = instance("migrate", 8);
        run(&storage, &executor, &mut inst).unwrap(); // пауза на human_review

        let mut new_process = parse(GOLDEN_FIXTURE).unwrap();
        new_process.version = 99;

        migrate_version(&storage, &mut inst, new_process).unwrap();

        assert_eq!(inst.process.version, 99);
        let events = storage.replay(&inst.id).unwrap();
        assert!(events.iter().any(|e| matches!(
            &e.kind,
            EventKind::VersionMigrated { from_version, to_version }
                if *from_version == 3 && *to_version == 99
        )));
    }

    #[test]
    fn migrate_version_rejects_a_graph_missing_the_current_step() {
        let (storage, mut inst, executor) = instance("migrate-incompatible", 8);
        run(&storage, &executor, &mut inst).unwrap(); // пауза на human_review

        let new_process = parse(UNRELATED_PROCESS_YAML).unwrap();

        let result = migrate_version(&storage, &mut inst, new_process);

        assert!(matches!(
            result,
            Err(EngineError::MigrationIncompatible { ref step_id }) if step_id == "human_review"
        ));
        // Отказ не должен был поменять версию инстанса.
        assert_eq!(inst.process.version, 3);
    }

    #[test]
    fn migrate_version_propagates_an_invalid_new_graph() {
        let (storage, mut inst, executor) = instance("migrate-bad-graph", 8);
        run(&storage, &executor, &mut inst).unwrap();

        let empty_process = Process {
            name: "p".into(),
            version: 99,
            steps: vec![],
            limits: inst.process.limits.clone(),
        };

        let result = migrate_version(&storage, &mut inst, empty_process);

        assert!(matches!(result, Err(EngineError::Graph(_))));
    }

    #[test]
    fn instance_can_resume_after_migrating_to_a_compatible_new_version() {
        let (storage, mut inst, executor) = instance("migrate-resume", 8);
        run(&storage, &executor, &mut inst).unwrap(); // пауза на human_review

        let mut new_process = parse(GOLDEN_FIXTURE).unwrap();
        new_process.version = 99;
        migrate_version(&storage, &mut inst, new_process).unwrap();

        // «ответ возобновляет выполнение» — тот же приём, что и без миграции.
        let outcome = run(&storage, &executor, &mut inst).unwrap();

        assert_eq!(outcome, RunOutcome::Finished);
    }

    #[test]
    fn recover_after_migration_restores_the_instance_on_the_new_version() {
        // low-risk (не human_gate): `recover()` выводит `current_step` из
        // последнего `StepApplied`, `human_gate` его не журналирует (тот
        // же пробел, что у `HumanGateOpened` без `step_id` — не эта
        // задача, см. заметку в handoff), поэтому здесь — путь, где
        // `current_step` однозначен без этого пробела.
        let (storage, mut inst, executor) = instance("migrate-recover", 2);
        run(&storage, &executor, &mut inst).unwrap();
        let mut new_process = parse(GOLDEN_FIXTURE).unwrap();
        new_process.version = 99;
        migrate_version(&storage, &mut inst, new_process.clone()).unwrap();

        let recovered = recover(&storage, new_process, inst.id.clone()).unwrap();

        assert_eq!(recovered.current_step.as_deref(), Some("answer"));
        assert_eq!(recovered.process.version, 99);
    }

    #[test]
    fn recover_with_the_old_version_after_migration_is_a_version_mismatch() {
        let (storage, mut inst, executor) = instance("migrate-stale-recover", 8);
        run(&storage, &executor, &mut inst).unwrap();
        let mut new_process = parse(GOLDEN_FIXTURE).unwrap();
        new_process.version = 99;
        let old_process = parse(GOLDEN_FIXTURE).unwrap();
        migrate_version(&storage, &mut inst, new_process).unwrap();

        let result = recover(&storage, old_process, inst.id.clone());

        assert!(matches!(result, Err(EngineError::VersionMismatch { .. })));
    }

    // --- P7: resume_after_human_gate_timeout --------------------------------

    fn instance_with_process(yaml: &str, id: &str) -> (SqliteEventLog, ProcessInstance) {
        let process = parse(yaml).unwrap();
        let storage = SqliteEventLog::open_in_memory().unwrap();
        let inst = instantiate(&storage, ProcessInstanceId(id.into()), process, json!({})).unwrap();
        (storage, inst)
    }

    #[test]
    fn timeout_with_default_fail_policy_errors_and_journals_the_event() {
        // Golden-фикстура не объявляет on_timeout — по умолчанию Fail.
        let (storage, mut inst, executor) = instance("timeout-fail", 8);
        run(&storage, &executor, &mut inst).unwrap(); // пауза на human_review

        let result =
            resume_after_human_gate_timeout(&storage, &executor, &mut inst, "human_review");

        assert!(matches!(
            result,
            Err(EngineError::HumanGateTimeout { step_id }) if step_id == "human_review"
        ));
        let events = storage.replay(&inst.id).unwrap();
        assert!(events.iter().any(|e| matches!(
            &e.kind,
            EventKind::HumanGateTimedOut { policy } if policy == "fail"
        )));
        // Техдолг TD1.4: Fail блокирует инстанс так же, как Escalate —
        // повторный `run()` не должен молча пройти мимо гейта.
        assert!(inst.blocked.is_some());
        let rerun = run(&storage, &executor, &mut inst);
        assert!(matches!(rerun, Err(EngineError::InstanceBlocked { .. })));
    }

    const BRANCH_ON_TIMEOUT_YAML: &str = "
process: p
version: 1
limits:
  max_steps: 10
  timeout: 10m
steps:
  - id: gate
    type: human_gate
    reason: \"ждём\"
    on_timeout:
      action: branch
      to: fallback
  - id: fallback
    type: sequential
";

    #[test]
    fn timeout_with_branch_policy_redirects_current_step() {
        let (storage, mut inst) = instance_with_process(BRANCH_ON_TIMEOUT_YAML, "timeout-branch");
        inst.current_step = Some("gate".into());
        // `fallback` — type: sequential, execute_single_step для него не
        // вызывает исполнителя вообще — любой сгодится.
        let executor = FakeExecutor { risk: 0 };

        resume_after_human_gate_timeout(&storage, &executor, &mut inst, "gate").unwrap();

        // Техдолг TD1.3: раньше здесь проверялось только, что
        // current_step ПЕРЕСТАВЛЕН на "fallback" — но с исходной
        // семантикой next_step это означало «fallback уже посещён»,
        // и следующий run() выполнил бы шаг ПОСЛЕ него, не сам fallback.
        // Теперь fallback реально исполнен (сам шаг ничего не мутирует —
        // sequential — но current_step отражает факт исполнения, не
        // просто ручную перестановку).
        assert_eq!(inst.current_step.as_deref(), Some("fallback"));
        assert!(inst.blocked.is_none());
    }

    const BRANCH_ON_TIMEOUT_TO_MUTATING_STEP_YAML: &str = "
process: p
version: 1
limits:
  max_steps: 10
  timeout: 10m
steps:
  - id: gate
    type: human_gate
    reason: \"ждём\"
    on_timeout:
      action: branch
      to: answer
  - id: answer
    type: tool
    tool: crm.reply
    args: {}
";

    /// Техдолг TD1.3: с МУТИРУЮЩИМ (не `sequential`) целевым шагом пропуск
    /// был бы напрямую наблюдаем как отсутствующий `StepApplied` — именно
    /// та форма регрессии, которую `timeout_with_branch_policy_redirects_current_step`
    /// (использующий `sequential`-фиктивный fallback) не мог бы поймать.
    #[test]
    fn timeout_with_branch_policy_actually_executes_a_mutating_target_step() {
        let (storage, mut inst) = instance_with_process(
            BRANCH_ON_TIMEOUT_TO_MUTATING_STEP_YAML,
            "timeout-branch-mutating",
        );
        inst.current_step = Some("gate".into());
        let executor = FakeExecutor { risk: 0 };

        resume_after_human_gate_timeout(&storage, &executor, &mut inst, "gate").unwrap();

        assert_eq!(inst.current_step.as_deref(), Some("answer"));
        assert_eq!(inst.state["answer"], json!({"reply": "готово"}));
        let events = storage.replay(&inst.id).unwrap();
        assert!(events.iter().any(|e| matches!(
            &e.kind,
            EventKind::StepApplied { step_id } if step_id == "answer"
        )));
    }

    const ESCALATE_ON_TIMEOUT_YAML: &str = "
process: p
version: 1
limits:
  max_steps: 10
  timeout: 10m
steps:
  - id: gate
    type: human_gate
    reason: \"ждём\"
    on_timeout:
      action: escalate
";

    #[test]
    fn timeout_with_escalate_policy_leaves_current_step_paused() {
        let (storage, mut inst) =
            instance_with_process(ESCALATE_ON_TIMEOUT_YAML, "timeout-escalate");
        inst.current_step = Some("gate".into());
        let executor = FakeExecutor { risk: 0 };

        resume_after_human_gate_timeout(&storage, &executor, &mut inst, "gate").unwrap();

        assert_eq!(
            inst.current_step.as_deref(),
            Some("gate"),
            "эскалация не должна сдвигать инстанс — процесс остаётся на паузе"
        );
        let events = storage.replay(&inst.id).unwrap();
        assert!(events.iter().any(|e| matches!(
            &e.kind,
            EventKind::HumanGateTimedOut { policy } if policy == "escalate"
        )));
        // Техдолг TD1.4: раньше «остаётся на паузе» было утверждением
        // doc-комментария, не поведением — ничего не мешало следующему
        // run() пройти мимо. Теперь `blocked` реально принуждает это.
        assert!(inst.blocked.is_some());
        let rerun = run(&storage, &executor, &mut inst);
        assert!(matches!(rerun, Err(EngineError::InstanceBlocked { .. })));
    }

    /// Техдолг TD1.4: блокировка обязана переживать `recover()` в НОВОМ
    /// процессе (`--resume`), не только жить в памяти текущего —
    /// `recover` реконструирует `blocked` из журнала, если последнее
    /// событие — `HumanGateTimedOut` с политикой, отличной от `branch`.
    #[test]
    fn recover_reconstructs_blocked_state_from_the_journal() {
        let (storage, mut inst) =
            instance_with_process(ESCALATE_ON_TIMEOUT_YAML, "timeout-escalate-recover");
        inst.current_step = Some("gate".into());
        let executor = FakeExecutor { risk: 0 };
        resume_after_human_gate_timeout(&storage, &executor, &mut inst, "gate").unwrap();

        let process = parse(ESCALATE_ON_TIMEOUT_YAML).unwrap();
        let mut recovered = recover(&storage, process, inst.id.clone()).unwrap();

        assert!(recovered.blocked.is_some());
        let rerun = run(&storage, &executor, &mut recovered);
        assert!(matches!(rerun, Err(EngineError::InstanceBlocked { .. })));
    }

    #[test]
    fn timeout_on_non_human_gate_step_is_an_error() {
        let (storage, mut inst, executor) = instance("timeout-wrong-step", 2);

        let result = resume_after_human_gate_timeout(&storage, &executor, &mut inst, "classify");

        assert!(matches!(result, Err(EngineError::NotAHumanGate(step)) if step == "classify"));
    }

    #[test]
    fn timeout_on_unknown_step_is_an_error() {
        let (storage, mut inst, executor) = instance("timeout-unknown-step", 2);

        let result =
            resume_after_human_gate_timeout(&storage, &executor, &mut inst, "no-such-step");

        assert!(matches!(
            result,
            Err(EngineError::Graph(graph::GraphError::UnknownStep(step))) if step == "no-such-step"
        ));
    }

    #[test]
    fn branch_policy_to_unknown_step_is_an_error() {
        const YAML: &str = "
process: p
version: 1
limits:
  max_steps: 10
  timeout: 10m
steps:
  - id: gate
    type: human_gate
    reason: \"ждём\"
    on_timeout:
      action: branch
      to: no-such-fallback
";
        let (storage, mut inst) = instance_with_process(YAML, "timeout-branch-bad-target");
        inst.current_step = Some("gate".into());
        let executor = FakeExecutor { risk: 0 };

        let result = resume_after_human_gate_timeout(&storage, &executor, &mut inst, "gate");

        assert!(matches!(
            result,
            Err(EngineError::Graph(graph::GraphError::UnknownStep(step))) if step == "no-such-fallback"
        ));
    }
}
