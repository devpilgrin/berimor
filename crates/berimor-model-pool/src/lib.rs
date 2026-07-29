//! `berimor-model-pool` — реестр моделей, классы, селектор провайдера.
//!
//! Источник: `ideal-agent-architecture.md` §3.10, ADR-0010, ADR-0011.
//! ROADMAP: E3 (реестр/селектор) · E4 (llama.cpp) · E5 (удалённые провайдеры).

use berimor_types::model::{ModelIdentity, ModelTier};

/// Выбор провайдера внутри класса — код по декларативной политике
/// (ADR-0011): локальный при равном классе → дешевейший удалённый в
/// пределах латентность-бюджета → фиксированный порядок. Никогда — вопрос к модели.
pub trait ProviderSelector {
    fn select(&self, tier: ModelTier, latency_budget_ms: Option<u64>) -> Option<ModelIdentity>;
}
