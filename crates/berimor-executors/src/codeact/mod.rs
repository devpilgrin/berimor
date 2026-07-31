//! CodeAct — модель пишет программу, изолированное выполнение в Wasmtime.
//!
//! Источник: `docs/arch/executors.md` §4, `ADR-0022`, `docs/arch/stack.md` §4.
//! ROADMAP: E6 (`wasm_host` — хост Wasmtime), E7 (`static_analysis` —
//! белый список идентификаторов), E8 (`executor` — лимиты песочницы,
//! реальный гость на QuickJS, `CodeActExecutor`, связывающий всё
//! перечисленное в один цикл «промпт → JS-текст → анализ →
//! песочница → Mediation») — все сделаны.

pub mod executor;
pub mod static_analysis;
pub mod wasm_host;

pub use executor::{CodeActError, CodeActExecutor};
pub use static_analysis::{analyze, StaticAnalysisError};
pub use wasm_host::{WasmHost, WasmHostError, WasmLimits};
