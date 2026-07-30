//! Типы шагов без модели: sequential/parallel/loop/branch/checkpoint.
//!
//! Источник: `docs/arch/process-engine.md` §2 (таблица типов шагов), §4
//! («Выполнение»: `step := graph.next(state)` — код, не модель). ROADMAP: P2.
//!
//! Область этого модуля — граф без исполнителей и без движка: дать
//! `compile` (проверка графа при загрузке — «компилируется в исполняемый
//! граф», дословно из §2) и `next_step` (чистая функция состояние → следующий
//! шаг). Подключение к журналу/Mediation/Capability — задача P3; join-барьер
//! `parallel` по неймспейсам `state.parallel.<step_id>` — задача P5 (§4:
//! «Параллельные шаги пишут в разделённые неймспейсы... join мержит по
//! барьеру после завершения всех ветвей») — здесь только видно, что фаза
//! наступила, само слияние не реализовано.

use berimor_types::step::{Process, StepKind};
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NextStep {
    Single(String),
    /// `parallel`: набор шагов для одновременного запуска. Что происходит
    /// после — join-барьер по `state.parallel.<step_id>` — не решается
    /// здесь (P5).
    Fork(Vec<String>),
    Finished,
}

#[derive(Debug, thiserror::Error)]
pub enum GraphError {
    #[error("процесс не содержит ни одного шага")]
    EmptyProcess,
    #[error("дублирующийся id шага: '{0}'")]
    DuplicateStepId(String),
    #[error("шаг с id '{0}' не найден в процессе")]
    UnknownStep(String),
    #[error("branch-шаг '{step_id}' ссылается на несуществующий шаг '{target}'")]
    DanglingBranchTarget { step_id: String, target: String },
    #[error("parallel-шаг '{step_id}' ссылается на несуществующий шаг '{target}'")]
    DanglingParallelBranch { step_id: String, target: String },
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
                    if !seen.contains(target.as_str()) {
                        return Err(GraphError::DanglingParallelBranch {
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
        StepKind::Parallel { branches } => Ok(NextStep::Fork(branches.clone())),
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

/// Путь вида `state.classify.risk` — ведущий `state.` часть синтаксиса
/// шаблонов (`process-engine.md` §2), сам объект состояния уже и есть то
/// дерево, на которое ссылается путь.
fn resolve_path<'a>(path: &str, state: &'a Value) -> Option<&'a Value> {
    let path = path.strip_prefix("state.").unwrap_or(path);
    let mut current = state;
    for segment in path.split('.') {
        current = current.as_object()?.get(segment)?;
    }
    Some(current)
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
    let value = resolve_path(on, state).ok_or_else(|| GraphError::ConditionEvaluation {
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
