//! Rego-правила capability-гейта (волна D, 0.41.0): внешняя политика
//! на языке Rego (OPA) поверх статических правил — через regorus
//! (интерпретатор на чистом Rust, in-process, без sidecar).
//!
//! Контракт политики: `package berimor`, правило `deny contains msg if
//! { ... }` — непустое множество = отказ (все сообщения — в reason).
//! Пустое множество или отсутствие правила — НЕТРАЛЬНО: решают
//! статические правила (политика может только запрещать строже, не
//! разрешать слабее — гейт остаётся детерминированным ядром).
//!
//! Факты (`input`): tool (имя инструмента), args (аргументы, уже
//! замаскированные вызывающим), mutates (флаг политики инструмента),
//! environment (строка из конфига `[gate] environment`, дефолт "dev").
//!
//! Ошибки: политика не парсится — отказ старта (конфигурация невалидна);
//! ошибка ВЫЧИСЛЕНИЯ — fail-closed (отказ с диагностикой), не
//! молчаливый пропуск.

use std::sync::{Arc, Mutex};

use berimor_types::capability::{CapabilityDecision, ConfirmationMode, ProposedAction};

use crate::CapabilityGate;

/// Гейт-обёртка: Rego-политика → статические правила.
pub struct RegoGate {
    inner: Arc<dyn CapabilityGate + Send + Sync>,
    engine: Mutex<regorus::Engine>,
    environment: String,
}

impl RegoGate {
    /// Скомпилировать политику из исходника; ошибка разбора — сразу,
    /// до первого действия.
    pub fn new(
        inner: Arc<dyn CapabilityGate + Send + Sync>,
        policy_source: &str,
        environment: String,
    ) -> Result<Self, String> {
        let mut engine = regorus::Engine::new();
        engine
            .add_policy("berimor.rego".to_string(), policy_source.to_string())
            .map_err(|err| format!("rego-политика не разобрана: {err}"))?;
        Ok(Self {
            inner,
            engine: Mutex::new(engine),
            environment,
        })
    }

    /// Загрузка из файла конфигурации.
    pub fn from_file(
        inner: Arc<dyn CapabilityGate + Send + Sync>,
        path: &std::path::Path,
        environment: String,
    ) -> Result<Self, String> {
        let source = std::fs::read_to_string(path)
            .map_err(|err| format!("rego-политика {}: {err}", path.display()))?;
        Self::new(inner, &source, environment)
    }
}

impl CapabilityGate for RegoGate {
    fn check(&self, action: &ProposedAction, mode: ConfirmationMode) -> CapabilityDecision {
        let input = serde_json::json!({
            "tool": action.tool,
            "args": action.args,
            "mutates": action.mutates,
            "environment": self.environment,
        });
        let mut engine = match self.engine.lock() {
            Ok(engine) => engine,
            Err(_) => {
                return CapabilityDecision::Deny {
                    reason: "rego: блокировка движка отравлена (fail-closed)".to_string(),
                }
            }
        };
        if let Ok(value) = regorus::Value::from_json_str(&input.to_string()) {
            engine.set_input(value);
        }
        match engine.eval_query("data.berimor.deny".to_string(), false) {
            Ok(result) => {
                let mut messages: Vec<String> = Vec::new();
                // Результат запроса — значение первого выражения:
                // множество строк (или undefined).
                if let Some(regorus::Value::Set(items)) = result
                    .result
                    .first()
                    .and_then(|r| r.expressions.first())
                    .map(|e| &e.value)
                {
                    for item in items.iter() {
                        messages.push(item.to_string());
                    }
                }
                if messages.is_empty() {
                    // Нейтрально: решают статические правила.
                    self.inner.check(action, mode)
                } else {
                    CapabilityDecision::Deny {
                        reason: format!("rego: {}", messages.join("; ")),
                    }
                }
            }
            Err(err) => CapabilityDecision::Deny {
                reason: format!("rego: ошибка вычисления политики (fail-closed): {err}"),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct AllowAll;
    impl CapabilityGate for AllowAll {
        fn check(&self, _action: &ProposedAction, _mode: ConfirmationMode) -> CapabilityDecision {
            CapabilityDecision::Allow
        }
    }

    const POLICY: &str = r#"
package berimor

deny contains msg if {
    input.tool == "terminal.exec"
    input.environment == "prod"
    msg := "terminal.exec запрещён в prod-окружении"
}

deny contains msg if {
    input.mutates
    input.tool == "files.write"
    contains(input.args.path, "/etc/")
    msg := sprintf("запись в системные пути запрещена: %s", [input.args.path])
}
"#;

    fn gate(env: &str) -> RegoGate {
        RegoGate::new(Arc::new(AllowAll), POLICY, env.to_string()).expect("политика компилируется")
    }

    fn action(tool: &str, args: serde_json::Value, mutates: bool) -> ProposedAction {
        ProposedAction {
            tool: tool.to_string(),
            args,
            mutates,
        }
    }

    #[test]
    fn deny_rule_blocks_terminal_in_prod() {
        let decision = gate("prod").check(
            &action("terminal.exec", serde_json::json!({"command": "ls"}), false),
            ConfirmationMode::Off,
        );
        match decision {
            CapabilityDecision::Deny { reason } => {
                assert!(reason.contains("prod"), "{reason}");
            }
            other => panic!("ожидался отказ: {other:?}"),
        }
    }

    #[test]
    fn neutral_policy_falls_through_to_static() {
        // В dev terminal.exec не запрещён политикой → внутренний гейт (Allow).
        let decision = gate("dev").check(
            &action("terminal.exec", serde_json::json!({"command": "ls"}), false),
            ConfirmationMode::Off,
        );
        assert!(matches!(decision, CapabilityDecision::Allow));
    }

    #[test]
    fn args_reach_the_policy() {
        let decision = gate("dev").check(
            &action(
                "files.write",
                serde_json::json!({"path": "/etc/hosts"}),
                true,
            ),
            ConfirmationMode::Off,
        );
        assert!(matches!(decision, CapabilityDecision::Deny { .. }));
        // Вне /etc — нейтрально → Allow.
        let decision = gate("dev").check(
            &action(
                "files.write",
                serde_json::json!({"path": "./out.txt"}),
                true,
            ),
            ConfirmationMode::Off,
        );
        assert!(matches!(decision, CapabilityDecision::Allow));
    }

    #[test]
    fn broken_policy_fails_at_startup() {
        let result = RegoGate::new(Arc::new(AllowAll), "package", "dev".to_string());
        assert!(result.is_err());
    }
}
