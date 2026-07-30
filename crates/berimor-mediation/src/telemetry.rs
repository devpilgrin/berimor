//! Телеметрия отказов Mediation.
//!
//! Источник: `docs/arch/mediation.md` §6. ROADMAP: M7.
//!
//! Две вещи: (1) какое событие журнала соответствует исходу Mediation —
//! `mediation.md` §2: «каждая стадия пишет событие: mediation.{parsed,
//! validated, committed, rejected}»; варианты уже существуют в
//! `berimor_types::event::EventKind` (заложены на F1, не использовались
//! до этой задачи); (2) агрегация доли отказов по (процесс, шаг, модель,
//! версия контракта) — прямая цитата из документа.
//!
//! Кто пишет событие в журнал — не этот модуль: здесь нет доступа к
//! `EventLog`, только к данным. Запись — дело вызывающего кода (будущая
//! интеграция P3+Mediation), этот модуль лишь определяет содержимое.

use berimor_types::{
    event::EventKind,
    mediation::{MediationOutcome, MediationStage},
    model::ModelTier,
};
use std::collections::BTreeMap;

/// Событие журнала, соответствующее исходу одной попытки Mediation.
pub fn outcome_to_event_kind<T>(outcome: &MediationOutcome<T>) -> EventKind {
    match outcome {
        MediationOutcome::Committed(_) => EventKind::MediationCommitted,
        MediationOutcome::Retry(rejection) => EventKind::MediationRejected {
            reason: rejection.reason.clone(),
        },
        MediationOutcome::Escalate { reason, .. } => EventKind::MediationRejected {
            reason: reason.clone(),
        },
    }
}

/// Ключ агрегации — дословно из `mediation.md` §6: «по процессу, шагу,
/// модели, версии контракта».
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct RejectionKey {
    pub process: String,
    pub step: String,
    pub model_tier: Option<ModelTier>,
    pub contract_version: u32,
}

/// Одна попытка Mediation — минимальная запись для агрегации, не полное
/// событие журнала (в нём не нужны ни `seq`, ни `ts_ms`, ни `payload`).
pub struct MediationAttempt {
    pub process: String,
    pub step: String,
    pub model_tier: Option<ModelTier>,
    pub contract_version: u32,
    pub rejected: bool,
    /// Дошла ли попытка до второго повтора (индикатор «контракт слишком
    /// сложен для класса модели» — mediation.md §6, третий пункт).
    pub reached_second_retry: bool,
}

impl MediationAttempt {
    pub fn from_outcome<T>(
        process: &str,
        step: &str,
        model_tier: Option<ModelTier>,
        contract_version: u32,
        outcome: &MediationOutcome<T>,
    ) -> Self {
        let (rejected, reached_second_retry) = match outcome {
            MediationOutcome::Committed(_) => (false, false),
            MediationOutcome::Retry(rejection) => (true, rejection.retries_remaining == 0),
            MediationOutcome::Escalate { escalated_from, .. } => {
                (true, *escalated_from != MediationStage::Policy)
            }
        };
        Self {
            process: process.to_string(),
            step: step.to_string(),
            model_tier,
            contract_version,
            rejected,
            reached_second_retry,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct RejectionStats {
    pub total: u32,
    pub rejected: u32,
    pub reached_second_retry: u32,
}

impl RejectionStats {
    /// Доля отказов — mediation.md §6, первый пункт: «рост отказов после
    /// смены версии модели = деградация → событие контура здоровья навыков».
    /// Само событие контура здоровья — вне этого модуля (навыки, отдельная
    /// подсистема), здесь только цифра, на которую это правило смотрит.
    pub fn rejection_rate(&self) -> f64 {
        if self.total == 0 {
            0.0
        } else {
            self.rejected as f64 / self.total as f64
        }
    }

    /// Доля, дошедшая до второго повтора — mediation.md §6, третий пункт.
    pub fn second_retry_rate(&self) -> f64 {
        if self.total == 0 {
            0.0
        } else {
            self.reached_second_retry as f64 / self.total as f64
        }
    }
}

pub fn aggregate(attempts: &[MediationAttempt]) -> BTreeMap<RejectionKey, RejectionStats> {
    let mut stats: BTreeMap<RejectionKey, RejectionStats> = BTreeMap::new();
    for attempt in attempts {
        let key = RejectionKey {
            process: attempt.process.clone(),
            step: attempt.step.clone(),
            model_tier: attempt.model_tier,
            contract_version: attempt.contract_version,
        };
        let entry = stats.entry(key).or_default();
        entry.total += 1;
        if attempt.rejected {
            entry.rejected += 1;
        }
        if attempt.reached_second_retry {
            entry.reached_second_retry += 1;
        }
    }
    stats
}

#[cfg(test)]
mod tests {
    use super::*;
    use berimor_types::mediation::MediationRejection;

    fn key(process: &str, step: &str) -> RejectionKey {
        RejectionKey {
            process: process.to_string(),
            step: step.to_string(),
            model_tier: Some(ModelTier::Weak),
            contract_version: 1,
        }
    }

    #[test]
    fn committed_outcome_maps_to_committed_event() {
        let outcome: MediationOutcome<()> = MediationOutcome::Committed(());
        assert!(matches!(
            outcome_to_event_kind(&outcome),
            EventKind::MediationCommitted
        ));
    }

    #[test]
    fn retry_outcome_maps_to_rejected_event_with_reason() {
        let outcome: MediationOutcome<()> = MediationOutcome::Retry(MediationRejection {
            stage: MediationStage::Parse,
            reason: "не JSON".into(),
            retries_remaining: 1,
        });
        match outcome_to_event_kind(&outcome) {
            EventKind::MediationRejected { reason } => assert_eq!(reason, "не JSON"),
            other => panic!("ожидался MediationRejected, получено {other:?}"),
        }
    }

    #[test]
    fn aggregates_rejection_rate_per_process_step_model_and_contract_version() {
        let attempts = vec![
            MediationAttempt {
                process: "card-delivery-support".into(),
                step: "classify".into(),
                model_tier: Some(ModelTier::Weak),
                contract_version: 1,
                rejected: false,
                reached_second_retry: false,
            },
            MediationAttempt {
                process: "card-delivery-support".into(),
                step: "classify".into(),
                model_tier: Some(ModelTier::Weak),
                contract_version: 1,
                rejected: true,
                reached_second_retry: false,
            },
            MediationAttempt {
                process: "card-delivery-support".into(),
                step: "answer".into(),
                model_tier: Some(ModelTier::Weak),
                contract_version: 1,
                rejected: false,
                reached_second_retry: false,
            },
        ];

        let stats = aggregate(&attempts);

        let classify_stats = stats[&key("card-delivery-support", "classify")];
        assert_eq!(classify_stats.total, 2);
        assert_eq!(classify_stats.rejected, 1);
        assert_eq!(classify_stats.rejection_rate(), 0.5);

        let answer_stats = stats[&key("card-delivery-support", "answer")];
        assert_eq!(answer_stats.total, 1);
        assert_eq!(answer_stats.rejection_rate(), 0.0);
    }

    #[test]
    fn tracks_second_retry_rate_separately_from_rejection_rate() {
        let attempts = vec![
            MediationAttempt {
                process: "p".into(),
                step: "s".into(),
                model_tier: Some(ModelTier::Weak),
                contract_version: 1,
                rejected: true,
                reached_second_retry: true,
            },
            MediationAttempt {
                process: "p".into(),
                step: "s".into(),
                model_tier: Some(ModelTier::Weak),
                contract_version: 1,
                rejected: true,
                reached_second_retry: false,
            },
        ];

        let stats = aggregate(&attempts)[&key("p", "s")];
        assert_eq!(stats.rejection_rate(), 1.0);
        assert_eq!(stats.second_retry_rate(), 0.5);
    }

    #[test]
    fn empty_stats_have_zero_rates_not_division_by_zero_panic() {
        let stats = RejectionStats::default();
        assert_eq!(stats.rejection_rate(), 0.0);
        assert_eq!(stats.second_retry_rate(), 0.0);
    }

    /// `MediationAttempt::from_outcome` — мост между `MediationOutcome`,
    /// который реально возвращает `pipeline::mediate` (M6), и записью для
    /// агрегации, без ручного дублирования логики на каждом вызове.
    #[test]
    fn from_outcome_derives_rejected_and_second_retry_flags() {
        let committed: MediationOutcome<()> = MediationOutcome::Committed(());
        let attempt = MediationAttempt::from_outcome("p", "s", None, 1, &committed);
        assert!(!attempt.rejected);
        assert!(!attempt.reached_second_retry);

        let last_retry: MediationOutcome<()> = MediationOutcome::Retry(MediationRejection {
            stage: MediationStage::Schema,
            reason: "x".into(),
            retries_remaining: 0,
        });
        let attempt = MediationAttempt::from_outcome("p", "s", None, 1, &last_retry);
        assert!(attempt.rejected);
        assert!(attempt.reached_second_retry);
    }
}
