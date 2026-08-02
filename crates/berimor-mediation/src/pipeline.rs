//! Логика повтора (до 2) и детерминированной эскалации.
//!
//! Источник: `docs/arch/mediation.md` §5. ROADMAP: M6.
//!
//! Связывает `parse` (M2) → `schema` (M3) → `policy` (M4) → `commit` (M5) в
//! один проход и решает, что делать с отказом на любой из стадий — по
//! таблице из документа буквально:
//!
//! | Стадия | Повтор | Эскалация |
//! |---|---|---|
//! | parse/schema | до 2 | человек / падение шага |
//! | policy | 0 («не лечится повтором») | человек |
//! | утечка секрета | 0 | падение процесса + событие безопасности |
//!
//! Что этот модуль **не делает**: не вызывает модель повторно сам. Реальный
//! повтор — это новый вызов модели с добавленной в подсказку причиной
//! отказа (`mediation.md` §5: «в подсказку добавляется текст ошибки
//! валидации») — сборка такой подсказки принадлежит E2 (`StructuredLLM`,
//! не реализовано). `mediate` — один проход на одну попытку; решение
//! Retry/Escalate и подсчёт попыток — то, что здесь реализовано и
//! протестировано; цикл, который на Retry вызывает модель заново с новой
//! подсказкой — задача интеграции E2+M6 позже.

use crate::{commit, parse, policy, schema};
use berimor_types::{
    contract::Contract,
    mediation::{MediationOutcome, MediationRejection, MediationStage},
    model::ModelTier,
};
use serde_json::Value;

/// До 2 повторов на parse/schema — mediation.md §5.
const MAX_RETRIES: u8 = 2;

/// Правила `policy`, специфичные для контракта и шага — собираются
/// вызывающим кодом (P3/будущая интеграция), не выводятся из типа
/// автоматически: то, какие поля — ссылки на состояние, знает конкретный
/// шаг процесса, не контракт вообще.
#[derive(Default)]
pub struct PolicyRules<'a> {
    pub cross_field: &'a [policy::CrossFieldRule],
    pub state_references: &'a [policy::StateReferenceCheck],
    pub known_secrets: &'a [&'a str],
}

/// Один проход `parse -> schema -> policy -> commit` для одной попытки.
/// `attempt` — сколько попыток уже было ДО этой (0 — первая попытка).
pub fn mediate<C: Contract>(
    step_id: &str,
    raw: &str,
    state: &Value,
    model_tier: Option<ModelTier>,
    rules: &PolicyRules,
    attempt: u8,
) -> MediationOutcome<commit::CommitOutcome> {
    let mut trace = Vec::new();
    mediate_traced::<C>(step_id, raw, state, model_tier, rules, attempt, &mut trace)
}

/// Тот же проход, но с трассой стадий (аудит 1.10, `mediation.md` §2:
/// «каждая стадия пишет событие»): успешный parse кладёт в `trace`
/// `MediationParsed`, успешная schema — `MediationValidated`; исход
/// (committed/rejected/security) — как прежде, мапится из
/// [`MediationOutcome`] (`telemetry::outcome_to_event_kind`). Запись в
/// журнал — дело вызывающего кода (те же `on_attempt`-хуки
/// исполнителей), конвейер лишь сообщает факты стадий.
pub fn mediate_traced<C: Contract>(
    step_id: &str,
    raw: &str,
    state: &Value,
    model_tier: Option<ModelTier>,
    rules: &PolicyRules,
    attempt: u8,
    trace: &mut Vec<berimor_types::event::EventKind>,
) -> MediationOutcome<commit::CommitOutcome> {
    let parsed = match parse::parse(raw) {
        Ok(value) => value,
        Err(err) => return retry_or_escalate(MediationStage::Parse, err.to_string(), attempt),
    };
    trace.push(berimor_types::event::EventKind::MediationParsed);

    let contract: C = match schema::validate(parsed.clone()) {
        Ok(contract) => contract,
        Err(err) => return retry_or_escalate(MediationStage::Schema, err.to_string(), attempt),
    };
    trace.push(berimor_types::event::EventKind::MediationValidated);

    // Утечка секрета — не просто отказ политики, а немедленная эскалация
    // без повтора и с отдельной причиной (mediation.md §5: «попытка
    // утечки секрета» — своя строка в таблице, отдельная от «нарушение
    // политики»: «0 повторов, падение процесса + событие безопасности»,
    // не «человек»), проверяется первой из трёх policy-проверок.
    // Техдолг TD1.5: раньше здесь тоже возвращался `Escalate` — тот же
    // вариант ТИПА, что обычное нарушение политики, хотя doc-таблица
    // выше требует другого исхода. `SecurityViolation` — отдельный
    // вариант, различимый вызывающим кодом БЕЗ разбора текста `reason`.
    if let Err(err) = policy::check_no_leaked_secrets(&parsed, rules.known_secrets) {
        return MediationOutcome::SecurityViolation {
            reason: err.to_string(),
        };
    }

    if let Err(err) = policy::check_cross_field_rules(&parsed, rules.cross_field) {
        // Нарушение политики — без повтора (mediation.md §5: «не лечится
        // повтором»), сразу эскалация к человеку.
        return MediationOutcome::Escalate {
            reason: err.to_string(),
            escalated_from: MediationStage::Policy,
        };
    }

    if let Err(err) = policy::check_state_references(&parsed, state, rules.state_references) {
        return MediationOutcome::Escalate {
            reason: err.to_string(),
            escalated_from: MediationStage::Policy,
        };
    }

    MediationOutcome::Committed(commit::commit(step_id, &contract, model_tier))
}

fn retry_or_escalate<T>(stage: MediationStage, reason: String, attempt: u8) -> MediationOutcome<T> {
    if attempt < MAX_RETRIES {
        MediationOutcome::Retry(MediationRejection {
            stage,
            reason,
            retries_remaining: MAX_RETRIES - attempt - 1,
        })
    } else {
        MediationOutcome::Escalate {
            reason,
            escalated_from: stage,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::SupportReply;

    fn state() -> Value {
        serde_json::json!({"user": {"card_id": "card_1029"}})
    }

    fn support_reply_rules() -> [policy::StateReferenceCheck; 1] {
        [policy::StateReferenceCheck {
            output_field: "card_id",
            state_path: "state.user.card_id",
        }]
    }

    #[test]
    fn well_formed_output_commits_on_first_attempt() {
        let raw = r#"{"card_id": "card_1029", "reply": "Готово."}"#;
        let checks = support_reply_rules();
        let rules = PolicyRules {
            state_references: &checks,
            ..Default::default()
        };

        let outcome =
            mediate::<SupportReply>("answer", raw, &state(), Some(ModelTier::Weak), &rules, 0);

        assert!(matches!(outcome, MediationOutcome::Committed(_)));
    }

    #[test]
    fn unparseable_output_retries_with_remaining_count() {
        let outcome = mediate::<SupportReply>(
            "answer",
            "не json вообще",
            &state(),
            None,
            &PolicyRules::default(),
            0,
        );

        match outcome {
            MediationOutcome::Retry(rejection) => {
                assert_eq!(rejection.stage, MediationStage::Parse);
                assert_eq!(rejection.retries_remaining, 1);
            }
            other => panic!("ожидался Retry, получено {other:?}"),
        }
    }

    #[test]
    fn parse_failure_on_last_attempt_escalates_instead_of_retrying() {
        // attempt = 2 значит уже было 2 попытки (MAX_RETRIES) — третьей не будет.
        let outcome = mediate::<SupportReply>(
            "answer",
            "не json вообще",
            &state(),
            None,
            &PolicyRules::default(),
            MAX_RETRIES,
        );

        match outcome {
            MediationOutcome::Escalate { escalated_from, .. } => {
                assert_eq!(escalated_from, MediationStage::Parse);
            }
            other => panic!("ожидался Escalate, получено {other:?}"),
        }
    }

    #[test]
    fn schema_violation_retries_up_to_the_limit() {
        // summary отсутствует у ClassificationOut — форма нарушена (M1/M3).
        let raw = r#"{"category": "billing"}"#;
        let outcome = mediate::<crate::contracts::ClassificationOut>(
            "classify",
            raw,
            &json_null(),
            None,
            &PolicyRules::default(),
            0,
        );
        assert!(matches!(
            outcome,
            MediationOutcome::Retry(MediationRejection {
                stage: MediationStage::Schema,
                ..
            })
        ));
    }

    #[test]
    fn policy_violation_never_retries_even_on_first_attempt() {
        let raw = r#"{"card_id": "card_9999_does_not_exist", "reply": "..."}"#;
        let checks = support_reply_rules();
        let rules = PolicyRules {
            state_references: &checks,
            ..Default::default()
        };

        // attempt = 0 — первая же попытка, но policy не лечится повтором
        // (mediation.md §5) — сразу Escalate, не Retry.
        let outcome = mediate::<SupportReply>("answer", raw, &state(), None, &rules, 0);

        match outcome {
            MediationOutcome::Escalate { escalated_from, .. } => {
                assert_eq!(escalated_from, MediationStage::Policy);
            }
            other => panic!("ожидался немедленный Escalate без повтора, получено {other:?}"),
        }
    }

    #[test]
    fn secret_leak_escalates_immediately_without_retry() {
        let raw = r#"{"card_id": "card_1029", "reply": "ваш токен sk-live-secret"}"#;
        let checks = support_reply_rules();
        let secrets = ["sk-live-secret"];
        let rules = PolicyRules {
            state_references: &checks,
            known_secrets: &secrets,
            ..Default::default()
        };

        let outcome = mediate::<SupportReply>("answer", raw, &state(), None, &rules, 0);

        // Техдолг TD1.5: раньше здесь был `Escalate{escalated_from: Policy}`
        // — тот же вариант, что обычное нарушение политики. Теперь —
        // отдельный вариант типа, различимый без разбора текста `reason`.
        assert!(matches!(
            outcome,
            MediationOutcome::SecurityViolation { .. }
        ));
    }

    fn json_null() -> Value {
        Value::Null
    }

    /// Аудит 1.10 / mediation.md §2: успешные стадии сообщают свои
    /// события — parsed после parse, validated после schema.
    #[test]
    fn successful_stages_are_traced() {
        use berimor_types::event::EventKind;
        let raw = r#"{"card_id": "card_1029", "reply": "Готово."}"#;
        let checks = support_reply_rules();
        let rules = PolicyRules {
            state_references: &checks,
            ..Default::default()
        };
        let mut trace = Vec::new();

        let outcome =
            mediate_traced::<SupportReply>("answer", raw, &state(), None, &rules, 0, &mut trace);

        assert!(matches!(outcome, MediationOutcome::Committed(_)));
        assert_eq!(
            trace,
            vec![EventKind::MediationParsed, EventKind::MediationValidated]
        );
    }

    /// Отказ на schema: parsed есть, validated — нет; отказ на parse —
    /// трасса пуста. По такой трассе «доля отказов по стадиям» из
    /// журнала достижима в точной форме (mediation.md §6).
    #[test]
    fn trace_stops_at_the_failing_stage() {
        use berimor_types::event::EventKind;
        let mut trace = Vec::new();
        let _ = mediate_traced::<SupportReply>(
            "answer",
            r#"{"card_id": 42}"#,
            &state(),
            None,
            &PolicyRules::default(),
            0,
            &mut trace,
        );
        assert_eq!(trace, vec![EventKind::MediationParsed]);

        let mut trace = Vec::new();
        let _ = mediate_traced::<SupportReply>(
            "answer",
            "не json",
            &state(),
            None,
            &PolicyRules::default(),
            0,
            &mut trace,
        );
        assert!(trace.is_empty());
    }
}
