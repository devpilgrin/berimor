//! Стадии Mediation и исходы валидации.
//!
//! Источник: `docs/arch/mediation.md` §2 («Поток»), §5 («Повторы и эскалация»).
//! ROADMAP: M2–M6.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MediationStage {
    Parse,
    Schema,
    Policy,
    Commit,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediationRejection {
    pub stage: MediationStage,
    pub reason: String,
    /// mediation.md §5: parse/schema — до 2 повторов; policy — 0
    /// («нарушение политики повтором не лечится»).
    pub retries_remaining: u8,
}

/// Итог одного прохода Mediation. `Escalate` всегда ведёт в `human_gate` —
/// путь «модель решила спросить человека» в архитектуре не существует
/// (эскалация — код, `mediation.md` §5).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MediationOutcome<T> {
    Committed(T),
    Retry(MediationRejection),
    Escalate {
        reason: String,
        escalated_from: MediationStage,
    },
}
