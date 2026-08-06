//! Стенд офлайн-оценки на золотых наборах: доля веток, доля отказов (O2).
//!
//! Источник: `ideal-agent-architecture.md` §3.11 («быстрый цикл — офлайн-
//! оценка процессных моделей на золотых наборах: доля веток, доля
//! отказов валидации»). ROADMAP: O2.
//!
//! Прогоняет каждый сценарий через `engine::run` с исполнителем, который
//! вызывающий код уже собрал (`StructuredLlm`+Mediation, `ToolOnly`,
//! тестовый fake — стенд не завязан на конкретный) и читает результат из
//! ЖУРНАЛА того же прогона — тот же источник данных, что у `trace`/
//! `replay_until` (O1), не отдельный параллельный сбор метрик:
//! `StepApplied` → какие шаги реально достигнуты (доля веток);
//! `MediationRejected`/`MediationCommitted` → доля отказов (M7,
//! `berimor_mediation::telemetry`). Если исполнитель не журналирует
//! Mediation-события сам (как это будет делать реальная связка
//! `StructuredLlm::on_attempt` + запись в тот же `EventLog`, `mediation.md`
//! §6) — доля отказов для сценария останется `0/0`, это ограничение
//! исполнителя, не стенда.
//!
//! Честная граница (v1): доля веток видит только непосредственные цели
//! `branch`-шагов, чьё достижение оставляет след в журнале (`StepApplied`
//! или остановка на `human_gate`, дающая `RunOutcome::AwaitingHuman`).
//! Цель, которая сама является дальнейшим control-flow-шагом
//! (`sequential`/`branch`/`checkpoint`), сейчас не разрешается глубже —
//! у единственной существующей golden-фикстуры (`card-delivery-support.yaml`)
//! обе цели `branch` — конечные шаги, разрешение глубже не требовалось;
//! не выдумано здесь заранее.

use berimor_process_engine::engine::{self, EngineError, RunOutcome, StepExecutor};
use berimor_storage::EventLog;
use berimor_types::event::{EventKind, ProcessInstanceId};
use berimor_types::step::{Process, StepKind};
use serde_json::Value;
use std::collections::HashSet;

/// Один сценарий золотого набора: вход процесса + исполнитель шагов.
pub struct GoldenScenario<'a> {
    pub name: String,
    pub input: Value,
    pub executor: &'a dyn StepExecutor,
}

/// Итог одного сценария.
pub struct ScenarioOutcome {
    pub name: String,
    pub result: Result<RunOutcome, EngineError>,
    /// Все `step_id`, реально достигнутые в этом прогоне.
    pub steps_reached: HashSet<String>,
    pub mediation_attempts: u32,
    pub mediation_rejections: u32,
}

/// Итог всего золотого набора.
pub struct EvalReport {
    pub scenarios: Vec<ScenarioOutcome>,
    /// Доля объявленных целей `branch`-шагов графа, реально достигнутых
    /// хотя бы одним сценарием набора. `1.0`, если в графе нет
    /// `branch`-шагов — нечему быть непокрытым, не `0/0`.
    pub branch_coverage: f64,
    /// Доля отказов Mediation по всему набору сразу
    /// (`MediationRejected` / общее число попыток). Точечная разбивка по
    /// (процесс, шаг, модель, версия контракта) — `berimor_mediation::telemetry`,
    /// вне этого стенда: из чистого журнала не восстановить модель/версию
    /// контракта попытки, только исход.
    pub failure_rate: f64,
}

/// Прогоняет весь набор сценариев по одному процессу и агрегирует метрики.
pub fn run_golden_set(
    storage: &dyn EventLog,
    process: &Process,
    scenarios: &[GoldenScenario],
) -> EvalReport {
    // Находка 4.9 аудита: id инстанса — «{процесс}::{сценарий}» — без
    // проверки уникальности имён два сценария сливали метрики в один
    // журнал. С 1.7 повторный instantiate — ошибка (громко, но поздно);
    // здесь — ранняя проверка с понятным сообщением ДО прогона.
    let mut seen = std::collections::HashSet::new();
    for scenario in scenarios {
        assert!(
            seen.insert(scenario.name.as_str()),
            "golden-набор '{}': дублирующееся имя сценария '{}' — метрики сценариев не должны смешиваться",
            process.name,
            scenario.name
        );
    }
    let scenarios: Vec<ScenarioOutcome> = scenarios
        .iter()
        .map(|scenario| run_scenario(storage, process, scenario))
        .collect();

    let branch_coverage = branch_coverage(process, &scenarios);
    let failure_rate = failure_rate(&scenarios);

    EvalReport {
        scenarios,
        branch_coverage,
        failure_rate,
    }
}

fn run_scenario(
    storage: &dyn EventLog,
    process: &Process,
    scenario: &GoldenScenario,
) -> ScenarioOutcome {
    let instance_id = ProcessInstanceId(format!("{}::{}", process.name, scenario.name));

    let result = engine::instantiate(
        storage,
        instance_id.clone(),
        process.clone(),
        scenario.input.clone(),
    )
    .and_then(|mut instance| engine::run(storage, scenario.executor, &mut instance));

    let mut steps_reached = HashSet::new();
    let mut mediation_attempts = 0u32;
    let mut mediation_rejections = 0u32;

    if let Ok(events) = storage.replay(&instance_id) {
        for event in &events {
            match &event.kind {
                EventKind::StepApplied { step_id } => {
                    steps_reached.insert(step_id.clone());
                }
                EventKind::MediationCommitted => mediation_attempts += 1,
                EventKind::MediationRejected { .. } => {
                    mediation_attempts += 1;
                    mediation_rejections += 1;
                }
                _ => {}
            }
        }
    }
    if let Ok(RunOutcome::AwaitingHuman { step_id, .. }) = &result {
        steps_reached.insert(step_id.clone());
    }

    ScenarioOutcome {
        name: scenario.name.clone(),
        result,
        steps_reached,
        mediation_attempts,
        mediation_rejections,
    }
}

fn branch_coverage(process: &Process, scenarios: &[ScenarioOutcome]) -> f64 {
    let targets: Vec<&str> = process
        .steps
        .iter()
        .filter_map(|step| match &step.kind {
            StepKind::Branch { cases, .. } => Some(cases.values().map(|s| s.as_str())),
            _ => None,
        })
        .flatten()
        .collect();
    if targets.is_empty() {
        return 1.0;
    }

    let reached: HashSet<&str> = scenarios
        .iter()
        .flat_map(|s| s.steps_reached.iter().map(|id| id.as_str()))
        .collect();
    let covered = targets
        .iter()
        .filter(|target| reached.contains(*target))
        .count();
    covered as f64 / targets.len() as f64
}

fn failure_rate(scenarios: &[ScenarioOutcome]) -> f64 {
    let total: u32 = scenarios.iter().map(|s| s.mediation_attempts).sum();
    let rejected: u32 = scenarios.iter().map(|s| s.mediation_rejections).sum();
    if total == 0 {
        0.0
    } else {
        rejected as f64 / total as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trace::trace;
    use berimor_process_engine::engine::ExecutorError;
    use berimor_storage::SqliteEventLog;
    use berimor_types::event::Event;
    use berimor_types::step::{Patch, Step};
    use serde_json::json;

    const GOLDEN_FIXTURE: &str =
        include_str!("../../../fixtures/golden/processes/card-delivery-support.yaml");

    fn process() -> Process {
        berimor_process_engine::parser::parse(GOLDEN_FIXTURE).unwrap()
    }

    /// Тот же приём, что `FakeExecutor` в `engine.rs` — детерминированный
    /// патч по id шага, без модели и без сети.
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

    /// Исполнитель, журналирующий Mediation-телеметрию как побочный
    /// эффект `execute()` — так это будет делать настоящая связка
    /// (`StructuredLlm::on_attempt` + запись вызывающим кодом,
    /// `mediation.md` §6). Стенд читает эти события из журнала уже после
    /// прогона, ничего не знает о конкретном исполнителе.
    struct MediatedFakeExecutor<'a> {
        storage: &'a dyn EventLog,
        instance_id: ProcessInstanceId,
        risk: i64,
    }

    impl StepExecutor for MediatedFakeExecutor<'_> {
        fn execute(&self, step: &Step, state: &Value) -> Result<Patch, ExecutorError> {
            if step.id == "classify" {
                let _ = self.storage.append(Event::new(
                    self.instance_id.clone(),
                    1,
                    EventKind::MediationRejected {
                        reason: "тестовый отказ первой попытки".into(),
                    },
                    json!({}),
                ));
                let _ = self.storage.append(Event::new(
                    self.instance_id.clone(),
                    1,
                    EventKind::MediationCommitted,
                    json!({}),
                ));
            }
            FakeExecutor { risk: self.risk }.execute(step, state)
        }
    }

    #[test]
    fn low_risk_scenario_reaches_fetch_card_status_not_human_review() {
        let storage = SqliteEventLog::open_in_memory().unwrap();
        let process = process();
        let scenario = GoldenScenario {
            name: "low-risk".into(),
            input: json!({"user": {"card_id": "c-1"}}),
            executor: &FakeExecutor { risk: 2 },
        };

        let report = run_golden_set(&storage, &process, std::slice::from_ref(&scenario));

        let outcome = &report.scenarios[0];
        assert!(matches!(outcome.result, Ok(RunOutcome::Finished)));
        assert!(outcome.steps_reached.contains("fetch_card_status"));
        assert!(!outcome.steps_reached.contains("human_review"));
    }

    #[test]
    fn high_risk_scenario_stops_at_human_review() {
        let storage = SqliteEventLog::open_in_memory().unwrap();
        let process = process();
        let scenario = GoldenScenario {
            name: "high-risk".into(),
            input: json!({"user": {"card_id": "c-1"}}),
            executor: &FakeExecutor { risk: 9 },
        };

        let report = run_golden_set(&storage, &process, std::slice::from_ref(&scenario));

        let outcome = &report.scenarios[0];
        assert!(matches!(
            outcome.result,
            Ok(RunOutcome::AwaitingHuman { ref step_id, .. }) if step_id == "human_review"
        ));
        assert!(outcome.steps_reached.contains("human_review"));
        assert!(!outcome.steps_reached.contains("fetch_card_status"));
    }

    #[test]
    fn branch_coverage_is_full_only_once_both_targets_are_reached_across_the_set() {
        let storage = SqliteEventLog::open_in_memory().unwrap();
        let process = process();
        let low = GoldenScenario {
            name: "low-risk".into(),
            input: json!({"user": {"card_id": "c-1"}}),
            executor: &FakeExecutor { risk: 2 },
        };

        let report_one = run_golden_set(&storage, &process, std::slice::from_ref(&low));
        assert_eq!(
            report_one.branch_coverage, 0.5,
            "покрыта только одна из двух целей check_risk"
        );

        let high = GoldenScenario {
            name: "high-risk".into(),
            input: json!({"user": {"card_id": "c-1"}}),
            executor: &FakeExecutor { risk: 9 },
        };
        let report_both = run_golden_set(&storage, &process, &[low, high]);
        assert_eq!(report_both.branch_coverage, 1.0);
    }

    #[test]
    fn process_without_branch_steps_has_full_coverage_by_definition() {
        let storage = SqliteEventLog::open_in_memory().unwrap();
        let mut process = process();
        process
            .steps
            .retain(|s| !matches!(s.kind, StepKind::Branch { .. }));

        let report = run_golden_set(&storage, &process, &[]);

        assert_eq!(report.branch_coverage, 1.0);
    }

    #[test]
    fn failure_rate_counts_rejections_journaled_by_the_executor() {
        let storage = SqliteEventLog::open_in_memory().unwrap();
        let process = process();
        let instance_id = ProcessInstanceId(format!("{}::mediated", process.name));
        let scenario = GoldenScenario {
            name: "mediated".into(),
            input: json!({"user": {"card_id": "c-1"}}),
            executor: &MediatedFakeExecutor {
                storage: &storage,
                instance_id,
                risk: 2,
            },
        };

        let report = run_golden_set(&storage, &process, std::slice::from_ref(&scenario));

        let outcome = &report.scenarios[0];
        assert_eq!(outcome.mediation_attempts, 2);
        assert_eq!(outcome.mediation_rejections, 1);
        assert_eq!(report.failure_rate, 0.5);
    }

    #[test]
    fn failure_rate_of_empty_set_is_zero_not_division_by_zero_panic() {
        let storage = SqliteEventLog::open_in_memory().unwrap();
        let process = process();

        let report = run_golden_set(&storage, &process, &[]);

        assert_eq!(report.failure_rate, 0.0);
        assert!(report.scenarios.is_empty());
    }

    #[test]
    fn eval_report_survives_a_failing_scenario_without_panicking() {
        struct AlwaysFails;
        impl StepExecutor for AlwaysFails {
            fn execute(&self, step: &Step, _state: &Value) -> Result<Patch, ExecutorError> {
                Err(ExecutorError {
                    step_id: step.id.clone(),
                    reason: "намеренный сбой теста".into(),
                })
            }
        }

        let storage = SqliteEventLog::open_in_memory().unwrap();
        let process = process();
        let scenario = GoldenScenario {
            name: "always-fails".into(),
            input: json!({"user": {"card_id": "c-1"}}),
            executor: &AlwaysFails,
        };

        let report = run_golden_set(&storage, &process, std::slice::from_ref(&scenario));

        assert!(report.scenarios[0].result.is_err());
        assert_eq!(report.branch_coverage, 0.0, "ни одна цель не достигнута");
    }

    #[test]
    fn scenario_outcome_trace_matches_eval_report_steps_reached() {
        // Не дублирующий параллельный сбор данных — доля веток стенда и
        // трассировка O1 читают один и тот же журнал.
        let storage = SqliteEventLog::open_in_memory().unwrap();
        let process = process();
        let scenario = GoldenScenario {
            name: "low-risk".into(),
            input: json!({"user": {"card_id": "c-1"}}),
            executor: &FakeExecutor { risk: 2 },
        };

        let report = run_golden_set(&storage, &process, std::slice::from_ref(&scenario));

        let instance_id = ProcessInstanceId(format!("{}::low-risk", process.name));
        let entries = trace(&storage, &instance_id).unwrap();
        let traced_steps: HashSet<String> = entries
            .iter()
            .filter(|e| e.kind == "step_applied")
            .map(|e| e.summary.clone())
            .collect();

        assert_eq!(traced_steps.len(), report.scenarios[0].steps_reached.len());
    }
}
