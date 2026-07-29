//! `berimor-executors` — ToolOnly, StructuredLLM, CodeAct, AgentStep.
//!
//! Источник: `arch/executors.md`. Каждый модуль — реализация
//! `berimor_types::executor::Executor` для одного из четырёх исполнителей
//! (`executors.md` §1). Ни один не пишет в состояние напрямую — только
//! через `berimor-mediation`.

pub mod agent_step;
pub mod codeact;
pub mod structured_llm;
pub mod tool_only;
