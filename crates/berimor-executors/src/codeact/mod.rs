//! CodeAct — модель пишет программу, изолированное выполнение в Wasmtime.
//!
//! Источник: `docs/arch/executors.md` §4, `ADR-0022`, `docs/arch/stack.md` §4.
//! ROADMAP: E6 (`wasm_host` — хост Wasmtime, сделано), E7
//! (`static_analysis` — белый список идентификаторов, сделано), E8
//! (лимиты песочницы + проводка результата через Mediation — не
//! сделано). `StepKind::CodeAct` не подключён к `CliExecutor` до E8 —
//! подключать пока нечего целиком: `analyze()` и `WasmHost` существуют
//! отдельно друг от друга и от вызывающего кода, который свяжет их в
//! один цикл «текст программы → анализ → компиляция/исполнение →
//! Mediation».

pub mod static_analysis;
pub mod wasm_host;

pub use static_analysis::{analyze, StaticAnalysisError};
pub use wasm_host::{WasmHost, WasmHostError};
