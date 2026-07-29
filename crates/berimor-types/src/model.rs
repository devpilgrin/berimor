//! Model Pool: классы способностей, идентичность провайдера, запрос/ответ.
//!
//! Источник: `ideal-agent-architecture.md` §3.10, ADR-0010, ADR-0011.
//! ROADMAP: E3–E5.

use serde::{Deserialize, Serialize};

/// Присваивается кодом реестра моделей по офлайн-оценке на золотом наборе,
/// не самой моделью (ADR-0010: «присвоение класса — код, не самооценка»).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ModelTier {
    Weak,
    Medium,
    Strong,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelIdentity {
    pub provider: String,
    pub model_id: String,
    pub tier: ModelTier,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionRequest {
    pub system_context: String,
    pub prompt: String,
    pub contract_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionResponse {
    pub raw_text: String,
    pub model: ModelIdentity,
}

#[derive(Debug, thiserror::Error)]
pub enum ModelError {
    #[error("провайдер недоступен: {0}")]
    Unavailable(String),
    #[error("бюджет исчерпан: {0}")]
    BudgetExceeded(String),
}
