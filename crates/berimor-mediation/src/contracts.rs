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

/// Пакет предложений фактов за одно извлечение (записной путь памяти,
/// memory-model.md §2/§4). Пустой пакет — законный ответ «запоминать
/// нечего» — модель не обязана выдумывать факт, чтобы заполнить форму.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(deny_unknown_fields)]
pub struct FactProposalBatch {
    #[validate(nested, length(max = 8))]
    pub facts: Vec<FactProposal>,
}

impl Contract for FactProposalBatch {
    const SCHEMA_VERSION: u32 = 1;
    const NAME: &'static str = "FactProposalBatch";
}

/// Ответ агента пользователю в интерактивном режиме `berimor chat`
/// (§20.11): финальный контракт `Finish.result` свободного цикла —
/// единственное текстовое поле, без доменной структуры (в отличие от
/// SupportReply, привязанного к сценарию поддержки).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(deny_unknown_fields)]
pub struct ChatReply {
    #[validate(length(min = 1, max = 8000))]
    pub reply: String,
}

impl Contract for ChatReply {
    const SCHEMA_VERSION: u32 = 1;
    const NAME: &'static str = "ChatReply";
}

impl Contract for FactProposal {
    const SCHEMA_VERSION: u32 = 1;
    const NAME: &'static str = "FactProposal";

    // Предложение факта — внутренний кандидат на запись, не то, что
    // предъявляется пользователю; публикуется (если вообще имеет смысл
    // публиковать факт) только после commit в семантический слой —
    // задача вызывающего кода, не этого контракта.
}

/// Решение одного хода `AgentStep` (`executors.md` §5: «рассуждение →
/// действие → наблюдение»). ROADMAP: E9. Фиксированная форма — ОДНА на
/// все `agent_step`-шаги системы, в отличие от `contract: String` самого
/// шага, который описывает форму ТОЛЬКО `Finish.result` (см.
/// `berimor_types::step::StepKind::AgentStep`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(deny_unknown_fields)]
pub struct AgentTurnDecision {
    /// Аудиторский след рассуждения — не публикуется пользователю
    /// (значение по умолчанию `Contract::publishable` — Null), но
    /// журналируется как часть попытки (M7).
    #[validate(length(min = 1, max = 4000))]
    pub thought: String,
    pub action: AgentAction,
}

/// `Value`-поля (`args`/`result`) — не форма, которую можно провалидировать
/// заранее: `args` проверяется тем же capability-гейтом, что и обычный
/// `tool`-шаг (`tool_only::dispatch_confirmed`), `result` — контрактом,
/// который декларирует `StepKind::AgentStep.contract`, отдельным проходом
/// Mediation после того, как модель выбрала `Finish`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AgentAction {
    Tool {
        tool: String,
        #[serde(default)]
        args: serde_json::Value,
    },
    Finish {
        result: serde_json::Value,
    },
}

impl Contract for AgentTurnDecision {
    const SCHEMA_VERSION: u32 = 1;
    const NAME: &'static str = "AgentTurnDecision";

    // Внутреннее решение цикла, не то, что видит пользователь —
    // публикуется только итоговый результат AgentStep (через контракт
    // самого шага), не промежуточные ходы.
}

/// Вердикт самокритики/проверки действия (`executors.md` §5: «модель
/// оценивает свой шаг» / «отдельный вердикт по критериям после
/// выполнения») — одна форма на обе стратегии, разный смысл
/// отрицательного вердикта решает вызывающий код
/// (`berimor-executors::agent_step`, не сам контракт).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(deny_unknown_fields)]
pub struct AgentVerdict {
    pub passed: bool,
    #[validate(length(min = 1, max = 2000))]
    pub reason: String,
}

impl Contract for AgentVerdict {
    const SCHEMA_VERSION: u32 = 1;
    const NAME: &'static str = "AgentVerdict";

    // Внутренний контроль цикла — не публикуется пользователю.
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

    #[test]
    fn agent_turn_decision_tool_action_round_trips() {
        let value = AgentTurnDecision {
            thought: "Нужен статус карты.".into(),
            action: AgentAction::Tool {
                tool: "crm.get_card_status".into(),
                args: json!({"id": "card_1029"}),
            },
        };
        let json = serde_json::to_value(&value).unwrap();
        let back: AgentTurnDecision = serde_json::from_value(json).unwrap();
        assert_eq!(value, back);
    }

    #[test]
    fn agent_turn_decision_finish_action_round_trips() {
        let value = AgentTurnDecision {
            thought: "Готово, можно завершать.".into(),
            action: AgentAction::Finish {
                result: json!({"reply": "готово"}),
            },
        };
        let json = serde_json::to_value(&value).unwrap();
        let back: AgentTurnDecision = serde_json::from_value(json).unwrap();
        assert_eq!(value, back);
    }

    #[test]
    fn agent_action_tag_distinguishes_tool_from_finish() {
        let tool_json = serde_json::to_value(AgentAction::Tool {
            tool: "x".into(),
            args: json!({}),
        })
        .unwrap();
        assert_eq!(tool_json["kind"], json!("tool"));

        let finish_json = serde_json::to_value(AgentAction::Finish { result: json!({}) }).unwrap();
        assert_eq!(finish_json["kind"], json!("finish"));
    }

    /// `deny_unknown_fields` на самом `AgentAction`, не только на
    /// объемлющем `AgentTurnDecision` — без него internally-tagged
    /// enum-вариант молча отбрасывает лишние поля (найдено независимым
    /// ревью E9).
    #[test]
    fn agent_action_tool_variant_rejects_unknown_field() {
        let raw = json!({"kind": "tool", "tool": "x", "args": {}, "unexpected": "smuggled"});
        let result: Result<AgentAction, _> = serde_json::from_value(raw);
        assert!(result.is_err());
    }

    #[test]
    fn agent_turn_decision_rejects_unknown_field() {
        let raw = json!({
            "thought": "x",
            "action": {"kind": "finish", "result": {}},
            "extra": true
        });
        let result: Result<AgentTurnDecision, _> = serde_json::from_value(raw);
        assert!(result.is_err());
    }

    #[test]
    fn agent_turn_decision_rejects_empty_thought() {
        use validator::Validate;
        let value = AgentTurnDecision {
            thought: String::new(),
            action: AgentAction::Finish { result: json!({}) },
        };
        assert!(value.validate().is_err());
    }

    #[test]
    fn agent_verdict_round_trips_and_rejects_unknown_field() {
        let value = AgentVerdict {
            passed: false,
            reason: "результат не отвечает на вопрос".into(),
        };
        let json = serde_json::to_value(&value).unwrap();
        let back: AgentVerdict = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(value, back);

        let mut with_extra = json;
        with_extra["extra"] = json!(1);
        let result: Result<AgentVerdict, _> = serde_json::from_value(with_extra);
        assert!(result.is_err());
    }
}
