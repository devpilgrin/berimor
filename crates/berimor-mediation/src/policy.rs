//! Стадия `policy`: межполевые правила, ссылки на состояние, контроль утечек.
//!
//! Источник: `docs/arch/mediation.md` §4.3. ROADMAP: M4.
//!
//! Детерминированная, без моделей — «модель судит модель» здесь так же
//! исключено, как и везде в архитектуре (ADR-0004). Три независимые
//! проверки, каждая — чистая функция:
//! - межполевые правила (`risk >= 7 => category != "other"`, дословный
//!   пример из документа);
//! - ссылки на состояние (модель не может «придумать» идентификатор,
//!   отличающийся от уже известного в состоянии);
//! - контроль утечек (четвёртая точка маскировки, в дополнение к трём
//!   границам данных из `security-model.md` §2).

use berimor_types::state_path;
use serde_json::Value;

#[derive(Debug, thiserror::Error)]
pub enum PolicyError {
    #[error("нарушено межполевое правило: {0}")]
    CrossField(String),
    #[error("ссылка на состояние не подтверждена: {0}")]
    StateReference(Box<StateReferenceDetail>),
    #[error("обнаружена попытка утечки секрета в выводе модели")]
    SecretLeak,
}

/// Детализация нарушенной ссылки на состояние — боксом внутри варианта
/// (clippy result_large_err: два `Value` раздувают `PolicyError` на
/// каждом `Result` в горячем пути политики).
#[derive(Debug, thiserror::Error)]
#[error("поле '{field}' = {claimed}, но в состоянии по пути '{state_path}' — {actual}")]
pub struct StateReferenceDetail {
    pub field: String,
    pub claimed: Value,
    pub state_path: String,
    pub actual: Value,
}

/// Межполевое правило — чистая функция от сериализованного вывода: `Ok`,
/// если правило соблюдено, `Err` с объяснением иначе. Пример из
/// `mediation.md` §4.3, буквально: `risk >= 7 => category != "other"`.
pub type CrossFieldRule = fn(&Value) -> Result<(), String>;

/// Проверка ссылки на состояние: значение поля `output_field` в выводе
/// обязано совпадать со значением по `state_path` — не просто
/// существовать, а именно совпадать: `mediation.md` §4.3 говорит «модель
/// не может придумать объект», то есть сослаться на что-то ДРУГОЕ, чем
/// уже известно из состояния, а не просто на что-то произвольное.
pub struct StateReferenceCheck {
    pub output_field: &'static str,
    pub state_path: &'static str,
}

pub fn check_cross_field_rules(
    output: &Value,
    rules: &[CrossFieldRule],
) -> Result<(), PolicyError> {
    for rule in rules {
        rule(output).map_err(PolicyError::CrossField)?;
    }
    Ok(())
}

pub fn check_state_references(
    output: &Value,
    state: &Value,
    checks: &[StateReferenceCheck],
) -> Result<(), PolicyError> {
    for check in checks {
        let claimed = output
            .get(check.output_field)
            .cloned()
            .unwrap_or(Value::Null);
        let actual = state_path::resolve(check.state_path, state)
            .cloned()
            .unwrap_or(Value::Null);

        if claimed != actual {
            return Err(PolicyError::StateReference(Box::new(
                StateReferenceDetail {
                    field: check.output_field.to_string(),
                    claimed,
                    state_path: check.state_path.to_string(),
                    actual,
                },
            )));
        }
    }
    Ok(())
}

/// Сканирует вывод на присутствие уже раскрытых значений секретов —
/// четвёртая точка маскировки (`security-model.md` §2, L5, в дополнение к
/// трём границам данных). Сравнение точное (подстрокой), не эвристический
/// поиск похожих значений — секрет либо буквально попал в текст, либо нет.
pub fn check_no_leaked_secrets(
    output: &Value,
    known_secret_values: &[&str],
) -> Result<(), PolicyError> {
    if known_secret_values.is_empty() {
        return Ok(());
    }
    if contains_any(output, known_secret_values) {
        return Err(PolicyError::SecretLeak);
    }
    Ok(())
}

fn contains_any(value: &Value, needles: &[&str]) -> bool {
    match value {
        Value::String(s) => needles.iter().any(|needle| s.contains(needle)),
        Value::Object(map) => map.values().any(|v| contains_any(v, needles)),
        Value::Array(items) => items.iter().any(|v| contains_any(v, needles)),
        _ => false,
    }
}

/// Правило из `mediation.md` §4.3, дословно: `risk >= 7 => category != "other"`.
pub fn classification_risk_requires_specific_category(output: &Value) -> Result<(), String> {
    let risk = output.get("risk").and_then(Value::as_u64);
    let category = output.get("category").and_then(Value::as_str);
    if let (Some(risk), Some(category)) = (risk, category) {
        if risk >= 7 && category == "other" {
            return Err(format!(
                "risk={risk} >= 7 требует category != 'other', получено '{category}'"
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const STATE_REFERENCE_FORGERY: &str =
        include_str!("../../../fixtures/golden/malicious-inputs/state-reference-forgery.json");

    #[test]
    fn cross_field_rule_accepts_high_risk_with_specific_category() {
        let output = json!({"risk": 8, "category": "debt"});
        assert!(check_cross_field_rules(
            &output,
            &[classification_risk_requires_specific_category]
        )
        .is_ok());
    }

    #[test]
    fn cross_field_rule_accepts_low_risk_with_any_category() {
        let output = json!({"risk": 2, "category": "other"});
        assert!(check_cross_field_rules(
            &output,
            &[classification_risk_requires_specific_category]
        )
        .is_ok());
    }

    #[test]
    fn cross_field_rule_rejects_high_risk_with_other_category() {
        let output = json!({"risk": 9, "category": "other"});
        let result =
            check_cross_field_rules(&output, &[classification_risk_requires_specific_category]);
        assert!(matches!(result, Err(PolicyError::CrossField(_))));
    }

    #[test]
    fn state_reference_accepts_matching_value() {
        let output = json!({"card_id": "card_1029"});
        let state = json!({"user": {"card_id": "card_1029"}});
        let checks = [StateReferenceCheck {
            output_field: "card_id",
            state_path: "state.user.card_id",
        }];
        assert!(check_state_references(&output, &state, &checks).is_ok());
    }

    /// Прогоняет буквально ту вредоносную фикстуру, что была подготовлена
    /// заранее для этой проверки: модель утверждает `card_id`, которого
    /// нет в состоянии (там записан другой) — policy обязана отклонить.
    #[test]
    fn rejects_state_reference_forgery_fixture() {
        #[derive(serde::Deserialize)]
        struct Fixture {
            state_snapshot: Value,
            raw_model_output: Value,
        }
        let fixture: Fixture = serde_json::from_str(STATE_REFERENCE_FORGERY).unwrap();

        let checks = [StateReferenceCheck {
            output_field: "card_id",
            state_path: "state.user.card_id",
        }];
        let result =
            check_state_references(&fixture.raw_model_output, &fixture.state_snapshot, &checks);

        match result {
            Err(PolicyError::StateReference(detail)) => {
                assert_eq!(detail.claimed, json!("card_9999_does_not_exist"));
                assert_eq!(detail.actual, json!("card_1029"));
            }
            other => panic!("ожидалась StateReference-ошибка, получено {other:?}"),
        }
    }

    #[test]
    fn state_reference_missing_from_state_entirely_is_rejected() {
        let output = json!({"card_id": "card_1029"});
        let state = json!({});
        let checks = [StateReferenceCheck {
            output_field: "card_id",
            state_path: "state.user.card_id",
        }];
        assert!(check_state_references(&output, &state, &checks).is_err());
    }

    #[test]
    fn secret_leak_detected_in_top_level_string() {
        let output = json!({"summary": "используйте токен sk-abc123 для доступа"});
        let result = check_no_leaked_secrets(&output, &["sk-abc123"]);
        assert!(matches!(result, Err(PolicyError::SecretLeak)));
    }

    #[test]
    fn secret_leak_detected_in_nested_field() {
        let output = json!({"details": {"note": "pwd=hunter2"}});
        let result = check_no_leaked_secrets(&output, &["hunter2"]);
        assert!(matches!(result, Err(PolicyError::SecretLeak)));
    }

    #[test]
    fn no_leak_when_output_is_clean() {
        let output = json!({"summary": "обычный ответ без секретов"});
        assert!(check_no_leaked_secrets(&output, &["sk-abc123"]).is_ok());
    }

    #[test]
    fn no_known_secrets_means_nothing_to_check() {
        let output = json!({"summary": "что угодно"});
        assert!(check_no_leaked_secrets(&output, &[]).is_ok());
    }
}
