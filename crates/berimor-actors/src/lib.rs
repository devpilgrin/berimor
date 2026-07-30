//! `berimor-actors` — модель актора, диспетчер, планировщик, шина событий.
//!
//! Источник: `ideal-agent-architecture.md` §3.8, ADR-0009. Координация —
//! топология процессной модели и правила диспетчера, не решение модели.
//!
//! Реализовано (Фаза 7, полностью): A1 (`actor`) · A2 (`signing`, `bus`) ·
//! A3 (`dispatcher`) · A4 (`dispatcher::Dispatcher::with_human_gate_limit`,
//! `scheduler::TickOutcome::Throttled`) · A5 (`scheduler`) · A6
//! (`actor::FreezeSwitch`).

pub mod actor;
pub mod bus;
pub mod dispatcher;
pub mod scheduler;
pub mod signing;
