//! Типы шагов без модели: sequential/parallel/loop/branch/checkpoint.
//!
//! Источник: `docs/arch/process-engine.md` §2 (таблица типов шагов), §4
//! («Выполнение»: `step := graph.next(state)` — код, не модель). ROADMAP: P2.
//!
//! Область этого модуля — граф без исполнителей и без движка: дать
//! `compile` (проверка графа при загрузке — «компилируется в исполняемый
//! граф», дословно из §2) и `next_step` (чистая функция состояние → следующий
//! шаг). Подключение к журналу/Mediation/Capability — задача P3.
//!
//! `parallel` (P5, §4: «параллельные шаги пишут в разделённые
//! неймспейсы... join мержит по барьеру после завершения всех ветвей») —
//! барьер решается ЗДЕСЬ, состоянием, а не отдельным полем движка:
//! `next_step` смотрит, какие ветви `parallel`-шага уже оставили патч в
//! `state.parallel.<fork_step_id>.<branch_step_id>` (реальное исполнение
//! ветвей — движок, P3/P5), и возвращает `Fork` с ТОЛЬКО оставшимися —
//! пустой остаток означает «барьер пройден», обычный переход к
//! следующему объявленному шагу, тот же путь, что у любого другого типа
//! шага. Один вызов `next_step` двигает не больше чем на одну ветвь за
//! раз — движок сам решает, сколько раз вызвать (`один писатель на
//! инстанс», §4 — синхронный цикл, не параллельные потоки).

use berimor_types::state_path;
use berimor_types::step::{Process, StepKind};
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NextStep {
    Single(String),
    /// `parallel`: ветви, ещё не оставившие патч в
    /// `state.parallel.<fork_step_id>.*` — не обязательно все объявленные
    /// ветви шага, если часть уже выполнена в предыдущих вызовах.
    Fork(Vec<String>),
    Finished,
}

#[derive(Debug, thiserror::Error)]
pub enum GraphError {
    #[error("процесс не содержит ни одного шага")]
    EmptyProcess,
    #[error("дублирующийся id шага: '{0}'")]
    DuplicateStepId(String),
    #[error("id шага '{0}' зарезервирован под служебный неймспейс состояния (state.{0}.*)")]
    ReservedStepId(String),
    #[error("шаг с id '{0}' не найден в процессе")]
    UnknownStep(String),
    #[error("branch-шаг '{step_id}' ссылается на несуществующий шаг '{target}'")]
    DanglingBranchTarget { step_id: String, target: String },
    #[error("parallel-шаг '{step_id}' ссылается на несуществующий шаг '{target}'")]
    DanglingParallelBranch { step_id: String, target: String },
    /// P5, v1: ветвь parallel-шага обязана быть мутирующим «листовым»
    /// шагом (`tool`/`llm_structured`/`codeact`/`agent_step`) —
    /// control-flow внутри ветви (`sequential`/`branch`/`checkpoint`/
    /// `human_gate`/`loop`/вложенный `parallel`) не разрешён в этой
    /// версии: неймспейс `state.parallel.<fork>.<branch>` рассчитан на
    /// один патч на ветвь, не на цепочку шагов с собственным
    /// управлением — расширение до цепочек является отдельной будущей
    /// задачей, не выдуманной здесь.
    #[error(
        "parallel-шаг '{step_id}': ветвь '{target}' не является мутирующим шагом (tool/llm_structured/codeact/agent_step)"
    )]
    ParallelBranchNotMutating { step_id: String, target: String },
    #[error("не удалось вычислить условие ветвления в шаге '{step_id}': {reason}")]
    ConditionEvaluation { step_id: String, reason: String },
    #[error("ни один case ветвления не совпал для шага '{step_id}' (значение состояния: {value})")]
    NoBranchMatched { step_id: String, value: String },
    #[error("шаг '{step_id}' не поддержан: {reason}")]
    Unsupported { step_id: String, reason: String },
}

/// Проверка графа при загрузке («компилируется в исполняемый граф» —
/// `process-engine.md` §2): пустой процесс, дублирующиеся id, ссылки
/// `branch`/`parallel` на несуществующие шаги — отказ здесь, до первого
/// исполнения, а не обнаружение на середине прогона.
pub fn compile(process: &Process) -> Result<(), GraphError> {
    if process.steps.is_empty() {
        return Err(GraphError::EmptyProcess);
    }

    let mut seen = std::collections::HashSet::new();
    for step in &process.steps {
        if !seen.insert(step.id.as_str()) {
            return Err(GraphError::DuplicateStepId(step.id.clone()));
        }
        // Находка 1.6 аудита: шаг с id 'parallel' своим патчем стирает
        // ВЕСЬ неймспейс барьера `state.parallel.*` вместе с результатами
        // выполненных ветвей — id зарезервирован под служебное
        // пространство, проверка здесь, не в рантайме.
        if step.id == "parallel" {
            return Err(GraphError::ReservedStepId(step.id.clone()));
        }
    }

    for step in &process.steps {
        match &step.kind {
            StepKind::Branch { cases, .. } => {
                for target in cases.values() {
                    if !seen.contains(target.as_str()) {
                        return Err(GraphError::DanglingBranchTarget {
                            step_id: step.id.clone(),
                            target: target.clone(),
                        });
                    }
                }
            }
            StepKind::Parallel { branches } => {
                for target in branches {
                    let Some(target_step) = process.steps.iter().find(|s| s.id == *target) else {
                        return Err(GraphError::DanglingParallelBranch {
                            step_id: step.id.clone(),
                            target: target.clone(),
                        });
                    };
                    if !is_mutating_leaf(&target_step.kind) {
                        return Err(GraphError::ParallelBranchNotMutating {
                            step_id: step.id.clone(),
                            target: target.clone(),
                        });
                    }
                }
            }
            _ => {}
        }
    }

    Ok(())
}

/// Шаги, которые вызывают исполнителя и производят ровно один патч —
/// единственные допустимые ветви `parallel` в v1 (см.
/// [`GraphError::ParallelBranchNotMutating`]).
fn is_mutating_leaf(kind: &StepKind) -> bool {
    matches!(
        kind,
        StepKind::Tool { .. }
            | StepKind::LlmStructured { .. }
            | StepKind::CodeAct { .. }
            | StepKind::AgentStep { .. }
    )
}

/// `state.classify.risk` → следующий шаг: `Single` для sequential-подобных
/// типов и branch, `Fork` для parallel, `Finished` после последнего шага.
/// `current = None` — старт процесса, первый шаг по порядку объявления.
pub fn next_step(
    process: &Process,
    current: Option<&str>,
    state: &Value,
) -> Result<NextStep, GraphError> {
    let current_index = match current {
        None => {
            return Ok(process
                .steps
                .first()
                .map(|s| NextStep::Single(s.id.clone()))
                .unwrap_or(NextStep::Finished));
        }
        Some(id) => process
            .steps
            .iter()
            .position(|s| s.id == id)
            .ok_or_else(|| GraphError::UnknownStep(id.to_string()))?,
    };

    let step = &process.steps[current_index];
    match &step.kind {
        StepKind::Branch { on, cases } => {
            let target = evaluate_branch(&step.id, on, cases, state)?;
            Ok(NextStep::Single(target))
        }
        StepKind::Parallel { branches } => {
            let remaining: Vec<String> = branches
                .iter()
                .filter(|branch| !parallel_branch_done(state, &step.id, branch))
                .cloned()
                .collect();
            if remaining.is_empty() {
                // Барьер пройден. Шаги-ветви объявлены в том же плоском
                // списке шагов процесса (другого места для их объявления
                // нет) — «следующий по индексу» шаг чаще всего оказался бы
                // одной из них, не настоящим продолжением. Продолжение —
                // первый шаг ПОСЛЕ текущего, чей id не входит в список
                // ветвей этого forка, независимо от того, где именно
                // ветви объявлены относительно него.
                Ok(process
                    .steps
                    .iter()
                    .skip(current_index + 1)
                    .find(|s| !branches.contains(&s.id))
                    .map(|s| NextStep::Single(s.id.clone()))
                    .unwrap_or(NextStep::Finished))
            } else {
                Ok(NextStep::Fork(remaining))
            }
        }
        StepKind::Loop { .. } => Err(GraphError::Unsupported {
            step_id: step.id.clone(),
            reason: "у Loop нет поля цели повтора — process-engine.md не даёт рабочего примера \
                     синтаксиса; открытый вопрос, не реализовано вместо угадывания структуры"
                .into(),
        }),
        // Sequential/Tool/LlmStructured/CodeAct/AgentStep/HumanGate/Checkpoint —
        // все они по умолчанию просто передают исполнение следующему
        // объявленному шагу; различие между ними — какой исполнитель
        // запускается (executors.md), не про то, куда идти дальше.
        _ => Ok(process
            .steps
            .get(current_index + 1)
            .map(|s| NextStep::Single(s.id.clone()))
            .unwrap_or(NextStep::Finished)),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Operator {
    Ge,
    Le,
    Gt,
    Lt,
    Eq,
    Ne,
}

/// Порядок операторов важен: двухсимвольные проверяются раньше
/// односимвольных (`>=` раньше `>`), иначе `>=7` разберётся как `>` с
/// литералом `=7`.
fn parse_case(case: &str) -> (Option<Operator>, &str) {
    const OPERATORS: &[(&str, Operator)] = &[
        (">=", Operator::Ge),
        ("<=", Operator::Le),
        ("==", Operator::Eq),
        ("!=", Operator::Ne),
        (">", Operator::Gt),
        ("<", Operator::Lt),
    ];
    for (prefix, op) in OPERATORS {
        if let Some(rest) = case.strip_prefix(prefix) {
            return (Some(*op), rest.trim());
        }
    }
    (None, case.trim())
}

fn case_matches(case: &str, value: &Value) -> Result<bool, String> {
    let (operator, literal) = parse_case(case);
    match operator {
        None => Ok(value_equals_literal(value, literal)),
        Some(op) => {
            let value_num = value
                .as_f64()
                .ok_or_else(|| format!("сравнение '{case}' требует числа, в состоянии {value}"))?;
            let literal_num: f64 = literal
                .parse()
                .map_err(|_| format!("не число после оператора в '{case}'"))?;
            Ok(match op {
                Operator::Ge => value_num >= literal_num,
                Operator::Le => value_num <= literal_num,
                Operator::Gt => value_num > literal_num,
                Operator::Lt => value_num < literal_num,
                Operator::Eq => value_num == literal_num,
                Operator::Ne => value_num != literal_num,
            })
        }
    }
}

fn value_equals_literal(value: &Value, literal: &str) -> bool {
    match value {
        Value::String(s) => s == literal,
        Value::Number(_) => value
            .as_f64()
            .zip(literal.parse::<f64>().ok())
            .is_some_and(|(a, b)| a == b),
        Value::Bool(b) => literal.parse::<bool>().ok() == Some(*b),
        _ => false,
    }
}

/// Ветвь `parallel`-шага уже выполнена, если под её ключом в
/// `state.parallel.<fork_step_id>` вообще что-то есть — сам факт наличия
/// ключа, не его значение (даже `null`/`false` от исполнителя — валидный
/// патч, не «ветвь не выполнена»).
fn parallel_branch_done(state: &Value, fork_step_id: &str, branch_step_id: &str) -> bool {
    state
        .get("parallel")
        .and_then(|p| p.get(fork_step_id))
        .and_then(|f| f.get(branch_step_id))
        .is_some()
}

/// Порядок обхода `cases` — по ключам `BTreeMap` (детерминированно), но не
/// содержателен для корректно построенного процесса: случаи ветвления
/// обязаны быть взаимоисключающими, автор процесса отвечает за это, не
/// движок (`branch` — I1: «модель никогда не выбирает ветку», но и код не
/// разрешает двусмысленность за автора).
fn evaluate_branch(
    step_id: &str,
    on: &str,
    cases: &BTreeMap<String, String>,
    state: &Value,
) -> Result<String, GraphError> {
    let value = state_path::resolve(on, state).ok_or_else(|| GraphError::ConditionEvaluation {
        step_id: step_id.to_string(),
        reason: format!("путь '{on}' не найден в состоянии"),
    })?;

    for (case, target) in cases {
        match case_matches(case, value) {
            Ok(true) => return Ok(target.clone()),
            Ok(false) => continue,
            Err(reason) => {
                return Err(GraphError::ConditionEvaluation {
                    step_id: step_id.to_string(),
                    reason,
                })
            }
        }
    }

    Err(GraphError::NoBranchMatched {
        step_id: step_id.to_string(),
        value: value.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse;
    use berimor_types::step::{ProcessLimits, Step};
    use serde_json::json;

    const GOLDEN_FIXTURE: &str =
        include_str!("../../../fixtures/golden/processes/card-delivery-support.yaml");

    fn golden() -> Process {
        parse(GOLDEN_FIXTURE).unwrap()
    }

    // --- compile() -----------------------------------------------------

    #[test]
    fn compile_accepts_golden_fixture() {
        assert!(compile(&golden()).is_ok());
    }

    #[test]
    fn compile_rejects_step_id_reserved_for_barrier_namespace() {
        // 1.6 аудита: id 'parallel' стирал бы state.parallel.* своим патчем.
        let mut process = golden();
        process.steps[0].id = "parallel".into();
        assert!(matches!(
            compile(&process),
            Err(GraphError::ReservedStepId(_))
        ));
    }

    #[test]
    fn compile_rejects_empty_process() {
        let process = Process {
            name: "empty".into(),
            version: 1,
            steps: vec![],
            limits: ProcessLimits {
                max_steps: 1,
                timeout_seconds: 1,
                token_budget: None,
                cost_budget: None,
                latency_budget_ms: None,
            },
        };
        assert!(matches!(compile(&process), Err(GraphError::EmptyProcess)));
    }

    #[test]
    fn compile_rejects_duplicate_step_ids() {
        let mut process = golden();
        let mut duplicate = process.steps[0].clone();
        duplicate.id = process.steps[1].id.clone();
        process.steps.push(duplicate);
        assert!(matches!(
            compile(&process),
            Err(GraphError::DuplicateStepId(_))
        ));
    }

    #[test]
    fn compile_rejects_dangling_branch_target() {
        let mut process = golden();
        if let StepKind::Branch { cases, .. } = &mut process
            .steps
            .iter_mut()
            .find(|s| s.id == "check_risk")
            .unwrap()
            .kind
        {
            cases.insert(">=100".into(), "no_such_step".into());
        }
        assert!(matches!(
            compile(&process),
            Err(GraphError::DanglingBranchTarget { .. })
        ));
    }

    #[test]
    fn compile_rejects_dangling_parallel_branch() {
        let mut process = golden();
        process.steps.push(Step {
            id: "fanout".into(),
            kind: StepKind::Parallel {
                branches: vec!["no_such_step".into()],
            },
        });
        assert!(matches!(
            compile(&process),
            Err(GraphError::DanglingParallelBranch { .. })
        ));
    }

    // --- next_step(): последовательность и переходы --------------------

    #[test]
    fn starts_at_first_declared_step() {
        let process = golden();
        assert_eq!(
            next_step(&process, None, &json!({})).unwrap(),
            NextStep::Single("classify".into())
        );
    }

    #[test]
    fn sequential_passthrough_after_llm_structured_step() {
        let process = golden();
        assert_eq!(
            next_step(&process, Some("classify"), &json!({})).unwrap(),
            NextStep::Single("check_risk".into())
        );
    }

    #[test]
    fn branch_high_risk_goes_to_human_review() {
        let process = golden();
        let state = json!({"classify": {"risk": 8}});
        assert_eq!(
            next_step(&process, Some("check_risk"), &state).unwrap(),
            NextStep::Single("human_review".into())
        );
    }

    #[test]
    fn branch_low_risk_goes_to_fetch_card_status() {
        let process = golden();
        let state = json!({"classify": {"risk": 2}});
        assert_eq!(
            next_step(&process, Some("check_risk"), &state).unwrap(),
            NextStep::Single("fetch_card_status".into())
        );
    }

    #[test]
    fn branch_boundary_value_matches_ge_case() {
        let process = golden();
        let state = json!({"classify": {"risk": 7}});
        assert_eq!(
            next_step(&process, Some("check_risk"), &state).unwrap(),
            NextStep::Single("human_review".into()),
            ">=7 включает границу 7"
        );
    }

    #[test]
    fn sequential_passthrough_after_human_gate() {
        let process = golden();
        assert_eq!(
            next_step(&process, Some("human_review"), &json!({})).unwrap(),
            NextStep::Single("fetch_card_status".into())
        );
    }

    #[test]
    fn sequential_passthrough_after_tool_step() {
        let process = golden();
        assert_eq!(
            next_step(&process, Some("fetch_card_status"), &json!({})).unwrap(),
            NextStep::Single("answer".into())
        );
    }

    #[test]
    fn finished_after_last_step() {
        let process = golden();
        assert_eq!(
            next_step(&process, Some("answer"), &json!({})).unwrap(),
            NextStep::Finished
        );
    }

    #[test]
    fn unknown_current_step_is_an_error_not_a_panic() {
        let process = golden();
        assert!(matches!(
            next_step(&process, Some("does_not_exist"), &json!({})),
            Err(GraphError::UnknownStep(_))
        ));
    }

    #[test]
    fn missing_state_path_is_an_error_not_a_panic() {
        let process = golden();
        let result = next_step(&process, Some("check_risk"), &json!({}));
        assert!(matches!(
            result,
            Err(GraphError::ConditionEvaluation { .. })
        ));
    }

    #[test]
    fn no_matching_case_is_an_error_not_a_silent_default() {
        let process = golden();
        // ни одна из фикстур cases (">=7", "<7") не покрывает нечисловое значение
        let state = json!({"classify": {"risk": "not-a-number"}});
        let result = next_step(&process, Some("check_risk"), &state);
        assert!(matches!(
            result,
            Err(GraphError::ConditionEvaluation { .. })
        ));
    }

    #[test]
    fn parallel_step_forks_to_its_branches() {
        let mut process = golden();
        process.steps.push(Step {
            id: "fanout".into(),
            kind: StepKind::Parallel {
                branches: vec!["classify".into(), "answer".into()],
            },
        });
        let result = next_step(&process, Some("fanout"), &json!({})).unwrap();
        assert_eq!(
            result,
            NextStep::Fork(vec!["classify".into(), "answer".into()])
        );
    }

    fn with_fanout(branches: Vec<&str>) -> Process {
        let mut process = golden();
        process.steps.push(Step {
            id: "fanout".into(),
            kind: StepKind::Parallel {
                branches: branches.into_iter().map(String::from).collect(),
            },
        });
        process
    }

    #[test]
    fn fork_lists_only_branches_not_yet_done() {
        let process = with_fanout(vec!["classify", "answer"]);
        let state = json!({"parallel": {"fanout": {"classify": {"risk": 1}}}});

        let result = next_step(&process, Some("fanout"), &state).unwrap();

        assert_eq!(result, NextStep::Fork(vec!["answer".into()]));
    }

    #[test]
    fn fork_barrier_passes_once_every_branch_is_done() {
        let process = with_fanout(vec!["classify", "answer"]);
        let state = json!({"parallel": {"fanout": {
            "classify": {"risk": 1},
            "answer": {"reply": "ok"}
        }}});

        let result = next_step(&process, Some("fanout"), &state).unwrap();

        // "fanout" объявлен последним шагом golden-процесса — после барьера
        // process.steps.get(index+1) не находит ничего дальше.
        assert_eq!(result, NextStep::Finished);
    }

    #[test]
    fn a_falsy_branch_result_still_counts_as_done() {
        // `false`/`null` — валидный патч, не "ветвь не выполнена":
        // проверка идёт по наличию ключа, не по его значению.
        let process = with_fanout(vec!["classify", "answer"]);
        let state = json!({"parallel": {"fanout": {
            "classify": false,
            "answer": {"reply": "ok"}
        }}});

        let result = next_step(&process, Some("fanout"), &state).unwrap();

        assert_eq!(result, NextStep::Finished);
    }

    #[test]
    fn compile_rejects_a_parallel_branch_that_is_not_a_mutating_leaf() {
        let process = with_fanout(vec!["check_risk"]); // check_risk — branch, не лист
        assert!(matches!(
            compile(&process),
            Err(GraphError::ParallelBranchNotMutating { ref step_id, ref target })
                if step_id == "fanout" && target == "check_risk"
        ));
    }

    #[test]
    fn compile_accepts_a_parallel_branch_that_is_a_mutating_leaf() {
        let process = with_fanout(vec!["classify", "answer"]);
        assert!(compile(&process).is_ok());
    }

    #[test]
    fn loop_step_is_explicitly_unsupported_not_silently_wrong() {
        let mut process = golden();
        process.steps.push(Step {
            id: "retry".into(),
            kind: StepKind::Loop {
                condition: "state.retries < 3".into(),
            },
        });
        let result = next_step(&process, Some("retry"), &json!({}));
        assert!(matches!(result, Err(GraphError::Unsupported { .. })));
    }

    // --- case_matches() как отдельная единица ---------------------------

    #[test]
    fn exact_match_case_without_operator() {
        assert!(case_matches("billing", &json!("billing")).unwrap());
        assert!(!case_matches("billing", &json!("debt")).unwrap());
    }

    #[test]
    fn comparison_case_requires_numeric_state_value() {
        let result = case_matches(">=7", &json!("not-a-number"));
        assert!(result.is_err());
    }
}
