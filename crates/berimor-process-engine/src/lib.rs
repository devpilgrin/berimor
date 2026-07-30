//! `berimor-process-engine` — детерминированный остов: граф шагов,
//! иммутабельное состояние, восстановление из журнала.
//!
//! Источник: `docs/arch/process-engine.md`. ROADMAP: P1–P8.
//!
//! - `parser` (P1) — декларация процесса → [`berimor_types::step::Process`] + хэш версии.
//! - `graph` (P2) — control-flow-типы шагов: чистая функция состояние → следующий шаг.
//! - `state` (F2) — атомарное применение патча, свёртка журнала в состояние.
//! - `engine` (P3) — цикл исполнения, соединяющий три модуля выше с журналом
//!   (`berimor-storage`) и с точкой расширения [`engine::StepExecutor`] —
//!   единственным местом, куда движок передаёт исполнение шагов с моделью.

pub mod engine;
pub mod graph;
pub mod parser;
pub mod state;

pub use engine::{EngineError, ProcessInstance, RunOutcome, StepExecutor};
