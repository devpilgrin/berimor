//! Контур здоровья навыков: рост отказов + неиспользование → событие
//! ревью, никогда молчаливая правка (O3).
//!
//! Источник: `ideal-agent-architecture.md` §3.11 («устаревание навыка
//! детектируется по росту отказов валидации и неиспользованию — событие
//! и очередь ревью человеком; никогда молчаливая правка»), `mediation.md`
//! §6. ROADMAP: O3.
//!
//! Чистая функция от готовой статистики использования к решению «нужен
//! ревью или нет» — сбор самой статистики (журнал вызовов навыка,
//! связь с MEM6-навыком по имени) не входит в эту задачу: та же граница,
//! что у O2 («стенд не завязан на конкретный исполнитель, читает то, что
//! ему дали»). `skill_name` здесь — обычная строка, совпадающая с
//! `SkillSummary::name`/`Skill::name` (MEM6) по соглашению, не по
//! типовой связи — модуль не зависит от `berimor-memory` для этого
//! (проверено тестом на реальном формате файла навыка).
//!
//! Результат — ДАННЫЕ ([`SkillHealthReviewEvent`]), не действие: что
//! делать с навыком (пометить устаревшим, переписать, оставить) —
//! решение человека в очереди ревью, не этого модуля.

use berimor_mediation::telemetry::RejectionStats;

/// Причина, по которой навык помечен на ревью — обе причины из §3.11
/// дословно, не изобретённые здесь.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillHealthConcern {
    /// Доля отказов текущего периода выросла относительно предыдущего
    /// не меньше чем на `HealthThresholds::rejection_rate_increase`.
    RejectionRateIncreased,
    /// Навык не использовался дольше `HealthThresholds::unused_after_days`.
    Unused,
}

/// Навык нуждается в ревью человеком.
#[derive(Debug, Clone, PartialEq)]
pub struct SkillHealthReviewEvent {
    pub skill_name: String,
    /// Может содержать обе причины сразу — они независимы.
    pub concerns: Vec<SkillHealthConcern>,
}

/// Пороги контура — код-правило, не решение модели (§5 таблицы решений
/// `ideal-agent-architecture.md`: «допуск к выполнению — код»).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HealthThresholds {
    pub rejection_rate_increase: f64,
    pub unused_after_days: u32,
}

impl Default for HealthThresholds {
    /// `0.2` (абсолютный рост доли отказов) и `30` дней — стартовые
    /// значения, не обоснованные ни одним документом эмпирически;
    /// вызывающий код может задать свои через явные поля.
    fn default() -> Self {
        Self {
            rejection_rate_increase: 0.2,
            unused_after_days: 30,
        }
    }
}

/// Статистика использования одного навыка за скользящее окно.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SkillUsage {
    /// Доля отказов предыдущего периода — база для сравнения роста.
    pub previous_rejection_rate: f64,
    pub current: RejectionStats,
    /// `None` — навык ещё ни разу не использовался (не то же самое, что
    /// «стал неиспользуемым»: новому навыку не давалась возможность
    /// использоваться, это не деградация — сознательно не считается
    /// поводом для ревью здесь).
    pub days_since_last_use: Option<u32>,
}

pub fn check_health(
    skill_name: &str,
    usage: &SkillUsage,
    thresholds: &HealthThresholds,
) -> Option<SkillHealthReviewEvent> {
    let mut concerns = Vec::new();

    if usage.current.total > 0 {
        let increase = usage.current.rejection_rate() - usage.previous_rejection_rate;
        if increase >= thresholds.rejection_rate_increase {
            concerns.push(SkillHealthConcern::RejectionRateIncreased);
        }
    }

    if let Some(days) = usage.days_since_last_use {
        if days >= thresholds.unused_after_days {
            concerns.push(SkillHealthConcern::Unused);
        }
    }

    if concerns.is_empty() {
        None
    } else {
        Some(SkillHealthReviewEvent {
            skill_name: skill_name.to_string(),
            concerns,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stats(total: u32, rejected: u32) -> RejectionStats {
        RejectionStats {
            total,
            rejected,
            reached_second_retry: 0,
        }
    }

    fn usage(
        previous_rate: f64,
        current: RejectionStats,
        days_since_last_use: Option<u32>,
    ) -> SkillUsage {
        SkillUsage {
            previous_rejection_rate: previous_rate,
            current,
            days_since_last_use,
        }
    }

    #[test]
    fn healthy_skill_needs_no_review() {
        let usage = usage(0.1, stats(10, 1), Some(1));

        assert_eq!(
            check_health("card-status-lookup", &usage, &HealthThresholds::default()),
            None
        );
    }

    #[test]
    fn rejection_rate_increase_past_threshold_triggers_review() {
        // 0.1 -> 0.6, рост 0.5 >= порога 0.2 по умолчанию.
        let usage = usage(0.1, stats(10, 6), Some(1));

        let event =
            check_health("card-status-lookup", &usage, &HealthThresholds::default()).unwrap();

        assert_eq!(event.skill_name, "card-status-lookup");
        assert_eq!(
            event.concerns,
            vec![SkillHealthConcern::RejectionRateIncreased]
        );
    }

    #[test]
    fn rejection_rate_increase_below_threshold_does_not_trigger() {
        // 0.1 -> 0.2, рост 0.1 < порога 0.2.
        let usage = usage(0.1, stats(10, 2), Some(1));

        assert_eq!(
            check_health("card-status-lookup", &usage, &HealthThresholds::default()),
            None
        );
    }

    #[test]
    fn unused_past_threshold_triggers_review() {
        let usage = usage(0.0, stats(0, 0), Some(45));

        let event =
            check_health("card-status-lookup", &usage, &HealthThresholds::default()).unwrap();

        assert_eq!(event.concerns, vec![SkillHealthConcern::Unused]);
    }

    #[test]
    fn never_used_skill_is_not_flagged_as_unused() {
        // None — навык ещё не имел возможности использоваться, не деградация.
        let usage = usage(0.0, stats(0, 0), None);

        assert_eq!(
            check_health("brand-new-skill", &usage, &HealthThresholds::default()),
            None
        );
    }

    #[test]
    fn both_concerns_can_fire_together() {
        let usage = usage(0.1, stats(10, 6), Some(45));

        let event =
            check_health("card-status-lookup", &usage, &HealthThresholds::default()).unwrap();

        assert_eq!(event.concerns.len(), 2);
        assert!(event
            .concerns
            .contains(&SkillHealthConcern::RejectionRateIncreased));
        assert!(event.concerns.contains(&SkillHealthConcern::Unused));
    }

    #[test]
    fn zero_total_attempts_does_not_compute_a_rate_increase() {
        // total=0 -> rejection_rate()=0.0 (M7: «без деления на ноль»), но
        // «нет попыток» — не «отказы выросли»: 0 отказов из 0 попыток не
        // повод для ревью только из-за previous_rejection_rate > 0.
        let usage = usage(0.9, stats(0, 0), Some(1));

        assert_eq!(
            check_health("card-status-lookup", &usage, &HealthThresholds::default()),
            None
        );
    }

    /// Интеграция с MEM6: `skill_name` — то же имя, что `Skill::name` из
    /// реального файла навыка, никакой параллельной схемы идентификации.
    #[test]
    fn skill_name_matches_the_name_field_of_a_real_skill_file() {
        let raw = "---\nname: card-status-lookup\nversion: 3\ndescription: Как проверить статус доставки карты клиента через CRM.\n---\n# Инструкция\n\n1. Вызови `crm.get_card_status` с id клиента.\n2. Сформулируй ответ по контракту SupportReply.\n";
        let skill = berimor_memory::procedural::parse_summary(raw).unwrap();

        let usage = usage(0.1, stats(10, 6), Some(1));
        let event = check_health(&skill.name, &usage, &HealthThresholds::default()).unwrap();

        assert_eq!(event.skill_name, skill.name);
    }
}
