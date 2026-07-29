//! `berimor-actors` — модель актора, диспетчер, планировщик.
//!
//! Источник: `ideal-agent-architecture.md` §3.8, ADR-0009. Координация —
//! топология процессной модели и правила диспетчера, не решение модели.

pub mod actor;
pub mod dispatcher;
pub mod scheduler;
