//! Стадии Mediation и исходы валидации.
//!
//! Источник: `docs/arch/mediation.md` §2 («Поток»), §5 («Повторы и эскалация»).
//! ROADMAP: M2–M6.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MediationStage {
    Parse,
    Schema,
    Policy,
    Commit,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum MediationOutcome<T> {
    Committed(T),
    Retry(MediationRejection),
    Escalate {
        reason: String,
        escalated_from: MediationStage,
    },
    /// Техдолг TD1.5 (`docs/audit-2026-07-31.md`): утечка секрета была
    /// неотличима от обычного отказа политики — обе давали
    /// `Escalate{escalated_from: Policy, ..}`, хотя doc-таблица
    /// `pipeline.rs` §5 требует разного исхода: «утечка секрета — 0
    /// повторов, падение процесса + событие безопасности», не «человек».
    /// Отдельный вариант ТИПА, не просто другой текст `reason`.
    SecurityViolation {
        reason: String,
    },
}
