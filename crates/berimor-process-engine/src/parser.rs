//! Парсер декларативного описания процесса и хэш версии.
//!
//! Источник: `docs/arch/process-engine.md` §2. «Модель версионируется; хэш
//! версии записывается в каждое событие выполнения, чтобы любой прогон
//! можно было воспроизвести и отнести к конкретной редакции процесса» —
//! это буквально «хэш», не только заявленное число `version` в декларации:
//! число можно забыть увеличить при правке графа, хэш содержимого — нет.
//! ROADMAP: P1.

use berimor_types::step::Process;
use sha2::{Digest, Sha256};

#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("не удалось разобрать декларацию процесса: {0}")]
    Yaml(#[from] serde_norway::Error),
}

/// Идентификатор конкретной редакции графа — SHA-256 канонической
/// сериализации разобранного [`Process`] (не сырого текста YAML:
/// эквивалентные по смыслу файлы с разным форматированием дают один хэш).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct VersionHash(pub String);

impl std::fmt::Display for VersionHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Разбирает декларацию процесса из YAML — толерантно к форматированию,
/// без эвристик поверх структуры (`mediation.md` §4.1 формулирует тот же
/// принцип для вывода модели: неразобранное — отказ, не догадка).
pub fn parse(yaml: &str) -> Result<Process, ParseError> {
    let process: Process = serde_norway::from_str(yaml)?;
    Ok(process)
}

/// Хэш версии — детерминированная функция от содержимого графа. Порядок
/// шагов в `Process.steps` и порядок ключей в `Branch.cases` (`BTreeMap`)
/// стабильны, поэтому хэш одного и того же по смыслу процесса стабилен
/// между запусками, машинами и временем — необходимое условие
/// воспроизводимости (I7), не только удобство.
pub fn version_hash(process: &Process) -> VersionHash {
    let canonical =
        serde_json::to_vec(process).expect("Process сериализуем без ошибок по построению типа");
    let digest = Sha256::digest(&canonical);
    VersionHash(to_hex(&digest))
}

fn to_hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use berimor_types::model::ModelTierRequirement;
    use berimor_types::step::StepKind;

    const GOLDEN_FIXTURE: &str =
        include_str!("../../../fixtures/golden/processes/card-delivery-support.yaml");

    #[test]
    fn parses_golden_fixture() {
        let process = parse(GOLDEN_FIXTURE).expect("золотая фикстура обязана разбираться");

        assert_eq!(process.name, "card-delivery-support");
        assert_eq!(process.version, 3);
        assert_eq!(process.steps.len(), 5);
        assert_eq!(process.limits.max_steps, 50);
    }

    #[test]
    fn parses_human_readable_limits() {
        let process = parse(GOLDEN_FIXTURE).unwrap();
        assert_eq!(process.limits.timeout_seconds, 600, "10m -> 600 секунд");
        assert_eq!(process.limits.token_budget, Some(100_000), "100k -> 100000");
    }

    #[test]
    fn parses_branch_step_with_ordered_cases() {
        let process = parse(GOLDEN_FIXTURE).unwrap();
        let check_risk = process
            .steps
            .iter()
            .find(|s| s.id == "check_risk")
            .expect("шаг check_risk должен существовать");

        match &check_risk.kind {
            StepKind::Branch { on, cases } => {
                assert_eq!(on, "state.classify.risk");
                assert_eq!(cases.get(">=7").map(String::as_str), Some("human_review"));
                assert_eq!(
                    cases.get("<7").map(String::as_str),
                    Some("fetch_card_status")
                );
            }
            other => panic!("ожидался Branch, получено {other:?}"),
        }
    }

    #[test]
    fn parses_human_gate_reason_field() {
        let process = parse(GOLDEN_FIXTURE).unwrap();
        let human_review = process
            .steps
            .iter()
            .find(|s| s.id == "human_review")
            .unwrap();
        match &human_review.kind {
            StepKind::HumanGate {
                reason_template,
                timeout_seconds,
                on_timeout,
            } => {
                assert_eq!(reason_template, "высокий риск: {{state.classify.risk}}");
                // Golden-фикстура не объявляет timeout/on_timeout (P7
                // добавлен позже) — значения по умолчанию (обратная
                // совместимость, `#[serde(default)]`).
                assert_eq!(*timeout_seconds, None);
                assert_eq!(
                    *on_timeout,
                    berimor_types::step::HumanGateTimeoutPolicy::Fail
                );
            }
            other => panic!("ожидался HumanGate, получено {other:?}"),
        }
    }

    #[test]
    fn parses_human_gate_with_explicit_timeout_and_branch_policy() {
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
    timeout: 10m
    on_timeout:
      action: branch
      to: fallback
  - id: fallback
    type: sequential
";
        let process = parse(YAML).unwrap();
        let gate = process.steps.iter().find(|s| s.id == "gate").unwrap();
        match &gate.kind {
            StepKind::HumanGate {
                timeout_seconds,
                on_timeout,
                ..
            } => {
                assert_eq!(*timeout_seconds, Some(600));
                assert_eq!(
                    *on_timeout,
                    berimor_types::step::HumanGateTimeoutPolicy::Branch {
                        to: "fallback".into()
                    }
                );
            }
            other => panic!("ожидался HumanGate, получено {other:?}"),
        }
    }

    #[test]
    fn parses_tool_step_with_args_template() {
        let process = parse(GOLDEN_FIXTURE).unwrap();
        let fetch = process
            .steps
            .iter()
            .find(|s| s.id == "fetch_card_status")
            .unwrap();
        match &fetch.kind {
            StepKind::Tool { tool, args } => {
                assert_eq!(tool, "crm.get_card_status");
                assert_eq!(args["id"], "{{state.user.card_id}}");
            }
            other => panic!("ожидался Tool, получено {other:?}"),
        }
    }

    #[test]
    fn parses_model_tier_any() {
        let process = parse(GOLDEN_FIXTURE).unwrap();
        let classify = process.steps.iter().find(|s| s.id == "classify").unwrap();
        match &classify.kind {
            StepKind::LlmStructured { model_tier, .. } => {
                assert_eq!(*model_tier, ModelTierRequirement::Any);
            }
            other => panic!("ожидался LlmStructured, получено {other:?}"),
        }
    }

    #[test]
    fn rejects_malformed_yaml_instead_of_guessing() {
        let result = parse("это: не: валидный: yaml: вообще: - - -");
        assert!(result.is_err());
    }

    #[test]
    fn version_hash_is_deterministic() {
        let process = parse(GOLDEN_FIXTURE).unwrap();
        let hash_a = version_hash(&process);
        let hash_b = version_hash(&process);
        assert_eq!(hash_a, hash_b);
        assert_eq!(hash_a.0.len(), 64, "SHA-256 в hex — 64 символа");
    }

    #[test]
    fn version_hash_changes_when_graph_changes_even_if_declared_version_does_not() {
        let mut process = parse(GOLDEN_FIXTURE).unwrap();
        let original_hash = version_hash(&process);

        // Меняем граф, не трогая process.version — ровно тот случай,
        // ради которого нужен хэш, а не только заявленное число.
        process.steps.push(berimor_types::step::Step {
            id: "extra".into(),
            kind: StepKind::Checkpoint,
        });

        let changed_hash = version_hash(&process);
        assert_ne!(
            original_hash, changed_hash,
            "изменение графа обязано менять хэш, даже если version не тронут"
        );
    }

    #[test]
    fn version_hash_stable_regardless_of_yaml_formatting() {
        let compact = "process: p\nversion: 1\nsteps: []\nlimits:\n  max_steps: 1\n  timeout: 1s\n";
        let spaced =
            "process:    p\nversion:   1\nsteps:     []\nlimits:\n  max_steps:   1\n  timeout:  1s\n";

        let a = version_hash(&parse(compact).unwrap());
        let b = version_hash(&parse(spaced).unwrap());
        assert_eq!(
            a, b,
            "хэш — от структуры, не от форматирования исходного YAML"
        );
    }
}
