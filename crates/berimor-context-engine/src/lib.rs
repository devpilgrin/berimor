//! `berimor-context-engine` — маршрутизатор → сборщик → оценщик бюджета.
//!
//! Источник: `ideal-agent-architecture.md` §3.5. ROADMAP: C1–C3.

use berimor_types::model::ModelTier;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ContextLayer {
    pub name: String,
    pub content: String,
    pub weight: f32,
}

/// `build(step, state) → context` — единственный путь чтения памяти в
/// структурированных шагах (`memory-model.md` §3): у модели нет инструмента
/// «сама поищи в памяти».
pub trait ContextBuilder {
    fn build(
        &self,
        step_kind: &str,
        tier: ModelTier,
        state: &serde_json::Value,
    ) -> Vec<ContextLayer>;
}
