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
/// card-delivery-support.yaml`, шаг `answer`). `reply` — минимальная,
/// явно предположительная форма, согласованная с тем, что уже использовал
/// фейковый исполнитель в тестах P3. `card_id` добавлен вместе с M4: без
/// него нечего проверять в `fixtures/golden/malicious-inputs/
/// state-reference-forgery.json` (фикстура была подготовлена заранее
/// именно под эту стадию, шаблон её `raw_model_output` уже содержал
/// `card_id` — контракт дополнен, чтобы соответствовать, не выдуман заново).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(deny_unknown_fields)]
pub struct SupportReply {
    /// Ссылка на состояние (`state.user.card_id`) — модель обязана
    /// повторить уже известный из состояния id, не придумать свой
    /// (mediation.md §4.3, проверяется на стадии `policy`, M4).
    pub card_id: String,
    pub reply: String,
}

impl Contract for SupportReply {
    const SCHEMA_VERSION: u32 = 1;
    const NAME: &'static str = "SupportReply";

    /// В отличие от `ClassificationOut`, это то, что видит пользователь —
    /// весь контракт публикуется, включая `card_id` (безвредная
    /// информация, которую пользователь и так знает — это его карта).
    fn publishable(&self) -> serde_json::Value {
        serde_json::json!({"card_id": self.card_id, "reply": self.reply})
    }
}

/// Сжатие рабочей памяти (`memory-model.md` §4: «история сворачивается
/// суммаризацией (модель + контракт)»). ROADMAP: MEM1.
///
/// Модель предлагает только текст сводки — какие именно записи истории
/// она покрывает, решает код (`berimor-memory::working::collapse`), не
/// модель: диапазон покрытия не то, что стоит доверять самоотчёту модели,
/// это код уже точно знает (он же и передал модели то, что сворачивает).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(deny_unknown_fields)]
pub struct WorkingMemorySummary {
    #[validate(length(min = 1, max = 4000))]
    pub summary: String,
}

impl Contract for WorkingMemorySummary {
    const SCHEMA_VERSION: u32 = 1;
    const NAME: &'static str = "WorkingMemorySummary";

    // Сводка — внутренний механизм экономии бюджета, не то, что
    // предъявляется пользователю напрямую; оригинал остаётся доступным в
    // эпизодической памяти (§4). Значение по умолчанию трейта — Null.
}

/// Предложение факта (`memory-model.md` §2: контракт «предложение факта»
/// {субъект, предикат, объект, уверенность, источник}, дословно все пять
/// полей). ROADMAP: MEM3.
///
/// Инвариант I1 («модель не решает, что помнить») не нарушается тем, что
/// модель заполняет эти поля: контракт — только ПРЕДЛОЖЕНИЕ. Запись в
/// семантический слой происходит лишь после дедупликации кода
/// (`berimor_memory::semantic::dedup`), которую модель не видит и не
/// контролирует — сам факт того, что предложение прошло валидацию формы,
/// ещё не значит «записано».
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(deny_unknown_fields)]
pub struct FactProposal {
    #[validate(length(min = 1, max = 200))]
    pub subject: String,
    #[validate(length(min = 1, max = 200))]
    pub predicate: String,
    #[validate(length(min = 1, max = 500))]
    pub object: String,
    #[validate(range(min = 0.0, max = 1.0))]
    pub confidence: f32,
    /// Обязателен (§2: «источник факта обязателен») — защита от
    /// отравления памяти начинается с того, что у факта вообще есть
    /// заявленное происхождение, не анонимное «модель так сказала».
    #[validate(length(min = 1, max = 200))]
    pub source: String,
}

impl Contract for FactProposal {
    const SCHEMA_VERSION: u32 = 1;
    const NAME: &'static str = "FactProposal";

    // Предложение факта — внутренний кандидат на запись, не то, что
    // предъявляется пользователю; публикуется (если вообще имеет смысл
    // публиковать факт) только после commit в семантический слой —
    // задача вызывающего кода, не этого контракта.
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
            card_id: "card_1029".into(),
            reply: "Готово.".into(),
        };
        let json = serde_json::to_value(&value).unwrap();
        let back: SupportReply = serde_json::from_value(json).unwrap();
        assert_eq!(value, back);
    }

    #[test]
    fn working_memory_summary_round_trips() {
        let value = WorkingMemorySummary {
            summary: "Клиент уточнял статус доставки карты, риск низкий.".into(),
        };
        let json = serde_json::to_value(&value).unwrap();
        let back: WorkingMemorySummary = serde_json::from_value(json).unwrap();
        assert_eq!(value, back);
    }

    #[test]
    fn working_memory_summary_rejects_empty_string() {
        use validator::Validate;
        let value = WorkingMemorySummary {
            summary: String::new(),
        };
        assert!(value.validate().is_err());
    }

    #[test]
    fn working_memory_summary_rejects_unknown_field() {
        let raw = json!({"summary": "текст", "extra": true});
        let result: Result<WorkingMemorySummary, _> = serde_json::from_value(raw);
        assert!(result.is_err());
    }

    fn fact() -> FactProposal {
        FactProposal {
            subject: "клиент c-1".into(),
            predicate: "предпочитает_канал".into(),
            object: "email".into(),
            confidence: 0.8,
            source: "session:run-1/step:answer".into(),
        }
    }

    #[test]
    fn fact_proposal_round_trips() {
        let value = fact();
        let json = serde_json::to_value(&value).unwrap();
        let back: FactProposal = serde_json::from_value(json).unwrap();
        assert_eq!(value, back);
    }

    #[test]
    fn fact_proposal_rejects_unknown_field() {
        let mut raw = serde_json::to_value(fact()).unwrap();
        raw["extra"] = json!(true);
        let result: Result<FactProposal, _> = serde_json::from_value(raw);
        assert!(result.is_err());
    }

    #[test]
    fn fact_proposal_rejects_empty_source() {
        use validator::Validate;
        let mut value = fact();
        value.source = String::new();
        assert!(
            value.validate().is_err(),
            "источник обязателен (memory-model.md §2)"
        );
    }

    #[test]
    fn fact_proposal_rejects_confidence_outside_unit_range() {
        use validator::Validate;
        let mut value = fact();
        value.confidence = 1.5;
        assert!(value.validate().is_err());
    }
}
