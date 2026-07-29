//! Границы между Executors, Mediation и Model Pool.
//!
//! Источник: `arch/executors.md` §1, `arch/mediation.md` §1.
//! ROADMAP: E1, E2, E6–E9.

use crate::{
    contract::Contract,
    mediation::MediationOutcome,
    model::{CompletionRequest, CompletionResponse, ModelError},
    step::{Patch, Step},
};

/// Один исполнитель шага. Реализации: `ToolOnly` / `StructuredLLM` /
/// `CodeAct` / `AgentStep` (`executors.md` §1). Исполнитель никогда не
/// пишет в состояние напрямую — возвращает сырой вывод, который обязан
/// пройти через [`MediationGate`].
pub trait Executor {
    type RawOutput;
    type Error: std::error::Error;

    fn run(&self, step: &Step, context: &serde_json::Value)
        -> Result<Self::RawOutput, Self::Error>;
}

/// Единственная точка, где сырой вывод исполнителя становится патчем
/// состояния (`mediation.md` §1: «ни один шаг с моделью не пишет в
/// состояние... напрямую»). Реализуется один раз в `berimor-mediation`,
/// используется всеми исполнителями.
pub trait MediationGate {
    fn commit<C: Contract>(&self, step_id: &str, raw_output: &str) -> MediationOutcome<Patch>;
}

/// Поставщик инференса — локальный (llama.cpp) или удалённый провайдер,
/// выбираемый Model Pool по классу и бюджету (ADR-0011).
pub trait ModelProvider {
    fn complete(&self, request: CompletionRequest) -> Result<CompletionResponse, ModelError>;
}
