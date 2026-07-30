//! `berimor-actors` — модель актора, диспетчер, планировщик.
//!
//! Источник: `ideal-agent-architecture.md` §3.8, ADR-0009. Координация —
//! топология процессной модели и правила диспетчера, не решение модели.
//!
//! Реализовано (Фаза 7): A1 (`actor`) · A3 (`dispatcher`) · A5 (`scheduler`) ·
//! A6 (`actor::FreezeSwitch`). Вне scope до соответствующих зависимостей:
//! A2 (подпись конвертов/ACL — ждёт S6, схему ACL-манифеста плагина) и A4
//! (лимит очереди `human_gate` — ждёт P7, шаг `human_gate` в Process
//! Engine с политикой таймаута эскалации).

pub mod actor;
pub mod dispatcher;
pub mod scheduler;
