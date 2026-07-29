//! Capability-решения: подтверждения, запреты, границы.
//!
//! Источник: `docs/arch/security-model.md` §2 (L3), §3 («Режимы подтверждений»).
//! ROADMAP: S1–S6.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConfirmationMode {
    Deny,
    Smart,
    Manual,
    Off,
}

/// Решение capability-слоя. `Deny` безусловен — security-model.md §3:
/// «подтверждение не отменяет». Ни один вызывающий код не может превратить
/// `Deny` в `Allow` — только повторная проверка самим capability-слоем
/// после изменения условий (например, сужения аргументов).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CapabilityDecision {
    Allow,
    Deny { reason: String },
    ConfirmRequired { reason: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProposedAction {
    pub tool: String,
    pub args: serde_json::Value,
    pub mutates: bool,
}
