//! `berimor-mediation` — parse → schema → policy → commit.
//!
//! Источник: `arch/mediation.md`. ROADMAP: M1–M7.

use berimor_types::{contract::Contract, mediation::MediationOutcome, step::Patch};

/// Реализация `berimor_types::executor::MediationGate` для конкретных
/// правил политики и телеметрии — единственная точка, где вывод модели
/// становится состоянием (инвариант I3).
pub trait MediationPipeline {
    fn parse(&self, raw: &str) -> Result<serde_json::Value, String>;
    fn validate_schema<C: Contract>(&self, value: serde_json::Value) -> Result<C, String>;
    fn check_policy<C: Contract>(
        &self,
        contract: &C,
        state: &serde_json::Value,
    ) -> Result<(), String>;
    fn commit(&self, step_id: &str, changes: serde_json::Value) -> MediationOutcome<Patch>;
}
