//! `berimor-storage` — единый встраиваемый движок хранения.
//!
//! Источник: `docs/arch/stack.md` §3, `docs/arch/memory-model.md`, ADR-0021: события,
//! снапшоты, полнотекст (FTS5), векторы (sqlite-vec) и граф сущностей — в
//! одном файле SQLite, а не в четырёх разных хранилищах.
//!
//! ROADMAP: F1 (события/снапшоты) · MEM2 (полнотекст) · MEM4 (векторы) · MEM7 (граф).

use berimor_types::event::{Event, EventSeq, ProcessInstanceId, Snapshot};

/// Единственный источник истины для журнала событий инстанса.
/// Реализация — SQLite, WAL, один писатель на инстанс (`process-engine.md` §4).
pub trait EventLog {
    fn append(&self, event: Event) -> Result<EventSeq, StorageError>;
    fn replay(&self, process_instance: &ProcessInstanceId) -> Result<Vec<Event>, StorageError>;
    fn latest_snapshot(
        &self,
        process_instance: &ProcessInstanceId,
    ) -> Result<Option<Snapshot>, StorageError>;
}

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("хранилище недоступно: {0}")]
    Unavailable(String),
    #[error("нарушение целостности журнала: {0}")]
    Corrupt(String),
}
