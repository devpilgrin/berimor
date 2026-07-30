//! Режимы подтверждений (`deny`/`smart`/`manual`/`off`) + декларация политики
//! на инструмент + композитный гейт, собирающий слои L3 в одну точку проверки.
//!
//! Источник: `docs/arch/security-model.md` §3 (таблица режимов и принципы),
//! `docs/arch/ideal-agent-architecture.md` §3.7 п.2. ROADMAP: S4.
//!
//! Порядок слоёв в [`StandardCapability::check`] — как в security-model.md §2:
//! сначала deny-статика (S1) — она безусловна и не отменяется никаким
//! режимом и никаким подтверждением (ADR-0007, I6); затем — режим
//! подтверждений с политикой конкретного инструмента. Jail (S2) и сетевой
//! гейт (S3) в этот метод не входят: jail применяется инструментом к
//! конкретному пути при обращении к ФС, сетевой гейт — HTTP-клиентом к
//! конкретному адресу; у обоих свой канал вызова, не `ProposedAction`.

use crate::deny;
use berimor_types::capability::{CapabilityDecision, ConfirmationMode, ProposedAction};
use std::collections::HashMap;
use std::path::PathBuf;

use crate::CapabilityGate;

/// Декларация политики инструмента — security-model.md §3: «у каждого
/// инструмента объявлено, требует ли действие подтверждения». Заполняется
/// конфигурацией/реестром инструментов, не выводится моделью.
#[derive(Debug, Clone, Default)]
pub struct ToolPolicy {
    /// Мутирует ли действие состояние вне процесса. Перекрывает флаг
    /// `ProposedAction.mutates`, когда политика инструмента известна —
    /// декларация точнее того, что смог сообщить вызывающий код.
    pub mutates: Option<bool>,
    /// Явное требование/освобождение от подтверждения. `Some(false)` —
    /// объявленная read-only операция («явно read-only» в режиме manual).
    pub requires_confirmation: Option<bool>,
    /// Деструктивное действие над внешней системой (базы данных, API,
    /// деплои): подтверждение ВСЕГДА, независимо от режима — снапшоты
    /// покрывают только локальные файлы, внешнюю систему откатить нельзя
    /// (security-model.md §3, «Принципы»).
    pub external_effect: bool,
}

/// Оценка действия по режиму подтверждений и политике инструмента.
/// Чистая функция; deny-статика сюда не входит — она выше, в гейте.
pub fn evaluate(
    mode: ConfirmationMode,
    action: &ProposedAction,
    policy: &ToolPolicy,
) -> CapabilityDecision {
    let mutates = policy.mutates.unwrap_or(action.mutates);

    // Внешний деструктив — всегда подтверждение, независимо от режима
    // (включая off и явное requires_confirmation=false: декларация
    // read-only не может отменить внешний эффект той же декларации).
    if mutates && policy.external_effect {
        return CapabilityDecision::ConfirmRequired {
            reason: format!(
                "'{}' — деструктивное действие над внешней системой: подтверждение обязательно в любом режиме",
                action.tool
            ),
        };
    }

    match mode {
        // deny как РЕЖИМ — безусловная блокировка любых мутаций; не путать
        // с deny-статикой классов операций (та безусловна во всех режимах).
        ConfirmationMode::Deny => {
            if mutates {
                CapabilityDecision::Deny {
                    reason: format!(
                        "режим deny: мутирующее действие '{}' заблокировано",
                        action.tool
                    ),
                }
            } else {
                CapabilityDecision::Allow
            }
        }
        // smart: чтение свободно, мутации — подтверждение; явная декларация
        // инструмента перекрывает вывод по флагу мутации.
        ConfirmationMode::Smart => match policy.requires_confirmation {
            Some(true) => CapabilityDecision::ConfirmRequired {
                reason: format!("'{}' объявлен требующим подтверждения", action.tool),
            },
            Some(false) => CapabilityDecision::Allow,
            None if mutates => CapabilityDecision::ConfirmRequired {
                reason: format!("'{}' — мутирующее действие (режим smart)", action.tool),
            },
            None => CapabilityDecision::Allow,
        },
        // manual: подтверждение на всё, кроме явно read-only.
        ConfirmationMode::Manual => {
            if !mutates || policy.requires_confirmation == Some(false) {
                CapabilityDecision::Allow
            } else {
                CapabilityDecision::ConfirmRequired {
                    reason: format!(
                        "'{}' — режим manual: подтверждение на всё, кроме read-only",
                        action.tool
                    ),
                }
            }
        }
        // off: без подтверждений — только изолированные профили, явное
        // включение владельцем (security-model.md §3). Deny-статика выше
        // при этом продолжает действовать.
        ConfirmationMode::Off => CapabilityDecision::Allow,
    }
}

/// Композитный гейт L3: deny-статика → режим подтверждений. Единственная
/// реализация [`CapabilityGate`] в ядре — вызывающий код не собирает слои
/// по частям и не может «забыть» deny-статику.
pub struct StandardCapability {
    workspace_root: PathBuf,
    tool_policies: HashMap<String, ToolPolicy>,
}

impl StandardCapability {
    pub fn new(workspace_root: PathBuf, tool_policies: HashMap<String, ToolPolicy>) -> Self {
        Self {
            workspace_root,
            tool_policies,
        }
    }
}

impl CapabilityGate for StandardCapability {
    fn check(&self, action: &ProposedAction, mode: ConfirmationMode) -> CapabilityDecision {
        // Слой 1: deny-статика — безусловна, до любых режимов (I6).
        if let Some(m) = deny::analyze(action, &self.workspace_root) {
            return CapabilityDecision::Deny {
                reason: format!("deny-статика ({}): {}", m.class.as_str(), m.evidence),
            };
        }
        // Слой 2: режим подтверждений с политикой инструмента; неизвестный
        // инструмент получает пустую политику — решение по флагу mutates.
        let policy = self
            .tool_policies
            .get(&action.tool)
            .cloned()
            .unwrap_or_default();
        evaluate(mode, action, &policy)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::PathBuf;

    fn action(tool: &str, mutates: bool) -> ProposedAction {
        ProposedAction {
            tool: tool.into(),
            args: json!({}),
            mutates,
        }
    }

    fn root() -> PathBuf {
        PathBuf::from("/workspace")
    }

    #[test]
    fn deny_static_wins_over_every_mode() {
        let gate = StandardCapability::new(root(), HashMap::new());
        let destructive = ProposedAction {
            tool: "terminal".into(),
            args: json!({"command": "rm -rf /"}),
            mutates: true,
        };
        for mode in [
            ConfirmationMode::Deny,
            ConfirmationMode::Smart,
            ConfirmationMode::Manual,
            ConfirmationMode::Off,
        ] {
            let decision = gate.check(&destructive, mode);
            assert!(
                matches!(decision, CapabilityDecision::Deny { .. }),
                "deny-статика обязана работать в любом режиме, {mode:?} вернул {decision:?}"
            );
        }
    }

    #[test]
    fn smart_mode_reads_free_mutations_confirm() {
        let policy = ToolPolicy::default();
        assert!(matches!(
            evaluate(ConfirmationMode::Smart, &action("crm.get", false), &policy),
            CapabilityDecision::Allow
        ));
        assert!(matches!(
            evaluate(
                ConfirmationMode::Smart,
                &action("crm.update", true),
                &policy
            ),
            CapabilityDecision::ConfirmRequired { .. }
        ));
    }

    #[test]
    fn manual_mode_confirms_everything_except_read_only() {
        let policy = ToolPolicy::default();
        assert!(matches!(
            evaluate(ConfirmationMode::Manual, &action("crm.get", false), &policy),
            CapabilityDecision::Allow
        ));
        assert!(matches!(
            evaluate(
                ConfirmationMode::Manual,
                &action("crm.update", true),
                &policy
            ),
            CapabilityDecision::ConfirmRequired { .. }
        ));
    }

    #[test]
    fn explicit_read_only_declaration_is_honored_in_smart_and_manual() {
        let policy = ToolPolicy {
            requires_confirmation: Some(false),
            ..Default::default()
        };
        for mode in [ConfirmationMode::Smart, ConfirmationMode::Manual] {
            assert!(matches!(
                evaluate(mode, &action("reports.build", true), &policy),
                CapabilityDecision::Allow
            ));
        }
    }

    #[test]
    fn external_effect_always_confirms_even_in_off_mode() {
        let policy = ToolPolicy {
            mutates: Some(true),
            external_effect: true,
            ..Default::default()
        };
        for mode in [
            ConfirmationMode::Deny,
            ConfirmationMode::Smart,
            ConfirmationMode::Manual,
            ConfirmationMode::Off,
        ] {
            let decision = evaluate(mode, &action("deploy.production", true), &policy);
            assert!(
                matches!(
                    decision,
                    CapabilityDecision::ConfirmRequired { .. } | CapabilityDecision::Deny { .. }
                ),
                "внешний деструктив не должен проходить свободно ни в одном режиме, {mode:?}"
            );
        }
        // А именно подтверждение (не deny) — во всех режимах, где deny-режим
        // сам по себе не строже.
        for mode in [ConfirmationMode::Smart, ConfirmationMode::Off] {
            assert!(matches!(
                evaluate(mode, &action("deploy.production", true), &policy),
                CapabilityDecision::ConfirmRequired { .. }
            ));
        }
    }

    #[test]
    fn deny_mode_blocks_mutations_allows_reads() {
        let policy = ToolPolicy::default();
        assert!(matches!(
            evaluate(ConfirmationMode::Deny, &action("crm.update", true), &policy),
            CapabilityDecision::Deny { .. }
        ));
        assert!(matches!(
            evaluate(ConfirmationMode::Deny, &action("crm.get", false), &policy),
            CapabilityDecision::Allow
        ));
    }

    #[test]
    fn off_mode_allows_but_does_not_cancel_deny_static() {
        let gate = StandardCapability::new(root(), HashMap::new());
        assert!(matches!(
            gate.check(&action("crm.update", true), ConfirmationMode::Off),
            CapabilityDecision::Allow
        ));
        // deny-статика — не режим, off её не отменяет (см. первый тест).
    }

    #[test]
    fn policy_mutates_declaration_overrides_action_flag() {
        let mut policies = HashMap::new();
        policies.insert(
            "crm.get_card_status".to_string(),
            ToolPolicy {
                mutates: Some(false),
                ..Default::default()
            },
        );
        let gate = StandardCapability::new(root(), policies);
        // Вызывающий код не знает природу инструмента и пессимистично
        // пометил действие мутирующим — декларация политики точнее.
        let decision = gate.check(
            &action("crm.get_card_status", true),
            ConfirmationMode::Smart,
        );
        assert!(matches!(decision, CapabilityDecision::Allow));
    }
}
