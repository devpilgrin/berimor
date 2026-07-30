//! `berimor-mediation` — parse → schema → policy → commit.
//!
//! Источник: `docs/arch/mediation.md`. ROADMAP: M1–M7.
//!
//! - `contracts` (M1) — конкретные типы контрактов (`ClassificationOut`, `SupportReply`).
//! - `parse` (M2) — снятие markdown-обёртки, без эвристик поверх содержимого.
//! - `schema` (M3) — диапазоны и длины сверх того, что проверяет serde derive.
//! - `policy` (M4) — межполевые правила, ссылки на состояние, контроль утечек.
//! - `commit` (M5) — патч / provenance-метаданные / публикуемые поля.
//! - `pipeline` (M6) — связывает всё выше в один проход с решением
//!   Retry/Escalate по таблице `mediation.md` §5.
//! - `telemetry` (M7) — событие журнала на исход + агрегация доли отказов.

pub mod commit;
pub mod contracts;
pub mod parse;
pub mod pipeline;
pub mod policy;
pub mod schema;
pub mod telemetry;
