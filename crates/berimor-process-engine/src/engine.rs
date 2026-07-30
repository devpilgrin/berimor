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
    })
}

/// Восстанавливает инстанс сверткой журнала (I7): `state = fold(events)`,
/// `current_step` — id шага последнего события `StepApplied`. `process` —
/// граф, который восстановлению нужно передать явно: журнал хранит только
/// номер версии (`process_version`), не сам граф — реестр процессных
/// версий (ADR-0012) не входит в область P3.
pub fn recover(
    storage: &dyn EventLog,
    process: Process,
    id: ProcessInstanceId,
) -> Result<ProcessInstance, EngineError> {
    graph::compile(&process)?;
    let events = storage.replay(&id)?;

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

    Ok(ProcessInstance {
        id,
        process,
        state,
        current_step,
    })
}

/// Прогоняет цикл `next → execute → apply → emit → snapshot` до
/// завершения, до `human_gate` или до превышения `max_steps`
/// (`token_budget`/`cost_budget`/`latency_budget_ms` — ROADMAP P6, здесь
/// не проверяются — Milestone 0 §3 признаёт этот минимум достаточным).
pub fn run(
    storage: &dyn EventLog,
    executor: &dyn StepExecutor,
    instance: &mut ProcessInstance,
) -> Result<RunOutcome, EngineError> {
    let mut steps_this_run: u32 = 0;

    loop {
        if steps_this_run >= instance.process.limits.max_steps {
            return Err(EngineError::LimitExceeded(format!(
                "max_steps = {}",
                instance.process.limits.max_steps
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
            graph::NextStep::Fork(_) => {
                return Err(EngineError::Unsupported(
                    "parallel: join-барьер по неймспейсам — задача P5, не реализовано".into(),
                ))
            }
            graph::NextStep::Single(id) => id,
        };

        let step = instance
            .process
            .steps
            .iter()
            .find(|s| s.id == step_id)
            .expect(
                "graph::next_step возвращает только id существующих шагов (гарантия compile())",
            );

        match &step.kind {
            StepKind::Sequential | StepKind::Branch { .. } => {
                // Ни то ни другое не мутирует состояние и не вызывает
                // исполнителя — сюда попадаем только чтобы сдвинуть
                // current_step; следующая итерация корректно разрешит
                // branch через graph::next_step с новым current.
            }
            StepKind::Checkpoint => {
                let seq = latest_seq(storage, &instance.id)?;
                storage.write_snapshot(Snapshot {
                    process_instance: instance.id.clone(),
                    seq,
                    state: instance.state.clone(),
                })?;
            }
            StepKind::HumanGate { reason_template } => {
                instance.current_step = Some(step_id);
                return Ok(RunOutcome::AwaitingHuman {
                    step_id: instance.current_step.clone().unwrap(),
                    // Резолвинг {{state...}} в шаблоне — забота Context
                    // Engine/Mediation (ещё не реализованы); отдаём как есть.
                    reason: reason_template.clone(),
                });
            }
            StepKind::Loop { .. } => {
                return Err(EngineError::Unsupported(
                    "loop: нет поля цели повтора — открытый вопрос архитектуры (см. graph.rs)"
                        .into(),
                ))
            }
            StepKind::Parallel { .. } => {
                return Err(EngineError::Unsupported(
                    "parallel как текущий шаг: join-барьер — задача P5, не реализовано".into(),
                ))
            }
            StepKind::Tool { .. }
            | StepKind::LlmStructured { .. }
            | StepKind::CodeAct { .. }
            | StepKind::AgentStep { .. } => {
                let patch = executor.execute(step, &instance.state)?;
                let event = Event::new(
                    instance.id.clone(),
                    instance.process.version,
                    EventKind::StepApplied {
                        step_id: step_id.clone(),
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

        instance.current_step = Some(step_id);
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse;
    use berimor_storage::SqliteEventLog;
    use serde_json::json;

    const GOLDEN_FIXTURE: &str =
        include_str!("../../../fixtures/golden/processes/card-delivery-support.yaml");

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
}
