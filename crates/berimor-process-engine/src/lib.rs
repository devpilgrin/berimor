//! `berimor-process-engine` — детерминированный остов: граф шагов,
//! иммутабельное состояние, восстановление из журнала.
//!
//! Источник: `docs/arch/process-engine.md`. ROADMAP: P1–P8.

use berimor_types::{
    event::ProcessInstanceId,
    step::{Patch, Process},
};

pub mod parser;
pub mod state;

/// Инстанс процесса — состояние + версия графа, зафиксированная при
/// создании на весь жизненный цикл (ADR-0012).
pub struct ProcessInstance {
    pub id: ProcessInstanceId,
    pub process: Process,
    pub state: serde_json::Value,
}

/// Цикл движка: `instantiate → (next → build → run → commit → apply → emit)*
/// → finish` (`process-engine.md` §4). Реализация подключается к
/// `berimor-storage` (журнал), `berimor-mediation` (валидация патча) и
/// `berimor-capability` (проверка перед мутацией) по мере выполнения P3–P8.
///
/// `apply` и `recover` — обёртки над [`state::apply_patch`] и [`state::fold`]
/// (F2, уже реализовано и протестировано); движку в P3 остаётся подключить
/// их к журналу `berimor-storage` и к графу шагов.
pub trait Engine {
    fn instantiate(
        &self,
        process: Process,
        input: serde_json::Value,
    ) -> Result<ProcessInstance, EngineError>;
    fn apply(&self, instance: &mut ProcessInstance, patch: Patch) -> Result<(), EngineError>;
    fn recover(&self, id: &ProcessInstanceId) -> Result<ProcessInstance, EngineError>;
}

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("превышен лимит процесса: {0}")]
    LimitExceeded(String),
    #[error("несовместимая версия процесса: {0}")]
    VersionMismatch(String),
}
