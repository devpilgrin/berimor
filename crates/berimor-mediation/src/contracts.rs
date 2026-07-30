//! Конкретные контракты — система типов, не только трейт.
//!
//! Источник: `docs/arch/mediation.md` §3 (`ClassificationOut` — дословный
//! пример из документа), golden-фикстура
//! `fixtures/golden/contracts/classification-out.json`. ROADMAP: M1.
//!
//! Здесь — только форма (поля, типы, `deny_unknown_fields`) и версия.
//! Диапазоны (`risk: min 0, max 10`) и ограничения длины
//! (`summary: max_length 280`) из того же примера — стадия `schema`
//! (M3), не типовая система: serde сам по себе не проверяет числовые
//! границы при десериализации, это отдельный проход после неё.

use berimor_types::contract::Contract;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use validator::Validate;

/// Дословно из `mediation.md` §3: `category: enum[billing, card, debt, other]`,
/// `risk: integer, min 0, max 10`, `summary: string, max_length 280`,
/// поля сверх перечисленных запрещены. Диапазон и длина — через
/// `#[validate(...)]` (M3): `u8` сам по себе допускает 0–255, серде их не
/// проверяет при десериализации, это отдельный проход после неё.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(deny_unknown_fields)]
pub struct ClassificationOut {
    pub category: Category,
    #[validate(range(min = 0, max = 10))]
    pub risk: u8,
    #[validate(length(max = 280))]
    pub summary: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    Billing,
    Card,
    Debt,
    Other,
}

impl Contract for ClassificationOut {
    const SCHEMA_VERSION: u32 = 1;
    const NAME: &'static str = "ClassificationOut";

    // Классификация — внутренние данные для ветвления (`state.classify.risk`
    // в примере процесса), не то, что показывают пользователю напрямую;
    // берётся значение по умолчанию трейта — Null, ничего не публикуется.
}

/// Второй контракт из golden-фикстуры процесса (`fixtures/golden/processes/
/// card-delivery-support.yaml`, шаг `answer`). Поля не описаны ни в одном
/// документе — минимальная, явно предположительная форма, согласованная
/// с тем, что уже использовал фейковый исполнитель в тестах P3 (`engine.rs`:
/// `"answer" => json!({"reply": "..."})`), а не выдуманная заново.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(deny_unknown_fields)]
pub struct SupportReply {
    pub reply: String,
}

impl Contract for SupportReply {
    const SCHEMA_VERSION: u32 = 1;
    const NAME: &'static str = "SupportReply";

    /// В отличие от `ClassificationOut`, это буквально то, что видит
    /// пользователь — весь контракт и есть публикуемая часть.
    fn publishable(&self) -> serde_json::Value {
        serde_json::json!({"reply": self.reply})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const MALICIOUS_UNKNOWN_FIELD: &str =
        include_str!("../../../fixtures/golden/malicious-inputs/unknown-field-injection.json");

    #[test]
    fn round_trips_through_json() {
        let value = ClassificationOut {
            category: Category::Billing,
            risk: 2,
            summary: "Обычный вопрос по счёту.".into(),
        };
        let json = serde_json::to_value(&value).unwrap();
        let back: ClassificationOut = serde_json::from_value(json).unwrap();
        assert_eq!(value, back);
    }

    #[test]
    fn category_serializes_lowercase_matching_the_docs_example() {
        let json = serde_json::to_value(Category::Billing).unwrap();
        assert_eq!(json, json!("billing"));
    }

    #[test]
    fn accepts_well_formed_output() {
        let raw = json!({
            "category": "billing",
            "risk": 2,
            "summary": "Обычный вопрос по счёту."
        });
        let result: Result<ClassificationOut, _> = serde_json::from_value(raw);
        assert!(result.is_ok());
    }

    /// Прогоняет буквально ту вредоносную фикстуру, что была подготовлена
    /// заранее для этой самой проверки (`fixtures/golden/README.md`:
    /// «обязана быть отклонена Mediation/Capability, а не пройти случайно»).
    #[test]
    fn rejects_unknown_field_injection_fixture() {
        #[derive(serde::Deserialize)]
        struct Fixture {
            raw_model_output: serde_json::Value,
        }
        let fixture: Fixture = serde_json::from_str(MALICIOUS_UNKNOWN_FIELD).unwrap();

        let result: Result<ClassificationOut, _> = serde_json::from_value(fixture.raw_model_output);

        assert!(
            result.is_err(),
            "поле сверх контракта обязано быть отклонено, а не тихо проигнорировано (deny_unknown_fields)"
        );
    }

    #[test]
    fn rejects_unknown_field_directly() {
        let raw = json!({
            "category": "billing",
            "risk": 2,
            "summary": "ok",
            "skip_human_review": true
        });
        let result: Result<ClassificationOut, _> = serde_json::from_value(raw);
        assert!(result.is_err());
    }

    #[test]
    fn rejects_unknown_enum_variant_not_silently_defaulted() {
        let raw = json!({"category": "not-a-real-category", "risk": 1, "summary": "x"});
        let result: Result<ClassificationOut, _> = serde_json::from_value(raw);
        assert!(result.is_err());
    }

    #[test]
    fn contract_identity_matches_process_declaration() {
        // Golden-процесс ссылается на контракт по имени (`contract:
        // ClassificationOut`) — NAME обязан совпадать буквально.
        assert_eq!(ClassificationOut::NAME, "ClassificationOut");
        assert_eq!(SupportReply::NAME, "SupportReply");
    }

    #[test]
    fn json_schema_exposes_declared_fields() {
        let schema = schemars::schema_for!(ClassificationOut);
        let schema_json = serde_json::to_value(&schema).unwrap();
        let properties = &schema_json["properties"];

        assert!(properties.get("category").is_some());
        assert!(properties.get("risk").is_some());
        assert!(properties.get("summary").is_some());
        assert!(
            schema_json.get("additionalProperties").is_some(),
            "схема должна отражать закрытость контракта хоть в каком-то поле метаданных"
        );
    }

    #[test]
    fn support_reply_round_trips() {
        let value = SupportReply {
            reply: "Готово.".into(),
        };
        let json = serde_json::to_value(&value).unwrap();
        let back: SupportReply = serde_json::from_value(json).unwrap();
        assert_eq!(value, back);
    }
}
