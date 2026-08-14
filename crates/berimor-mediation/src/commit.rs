//! Стадия `commit`: разделение патч / provenance-метаданные / публикуемые поля.
//!
//! Источник: `docs/arch/mediation.md` §4.4. ROADMAP: M5.
//!
//! `commit` вызывается только после того, как `policy` (M4, `policy.rs`)
//! пропустила вывод — коммит сам по себе не проверяет межполевые правила
//! или ссылки на состояние, это ответственность предыдущей стадии.
//! Собранный здесь `Patch` полагается на то, что содержимое `contract`
//! уже прошло весь путь `parse -> schema -> policy` (см. `pipeline.rs`, M6,
//! который и связывает стадии в одну последовательность с повторами).

use berimor_types::{contract::Contract, model::ModelTier, step::Patch};
use serde_json::Value;

/// Метаданные происхождения патча (mediation.md §4.4: «версия контракта,
/// идентификатор и класс модели»). Не часть состояния — состояние несёт
/// только `Patch`, эти поля нужны событию/телеметрии (M7), не бизнес-данным
/// в `state.<step_id>`.
#[derive(Debug, Clone, PartialEq)]
pub struct Provenance {
    pub contract_name: &'static str,
    pub contract_version: u32,
    /// `None` — коммит не с шага, требующего модели (не должно происходить
    /// в реальном использовании: `commit` вызывается только после
    /// структурированного вывода модели, но тип не запрещает вызвать его
    /// иначе — вызывающий код передаёт то, что действительно знает).
    pub model_tier: Option<ModelTier>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CommitOutcome {
    pub patch: Patch,
    pub provenance: Provenance,
    /// Только поля, помеченные контрактом как публикуемые
    /// ([`Contract::publishable`]) — не весь контракт целиком.
    pub publishable: Value,
}

/// Собирает патч из уже провалидированного контракта (прошедшего M2+M3).
/// `changes` — сериализация всего контракта целиком: то, что видит
/// состояние (`state.<step_id>`), не сужается — сужается только то, что
/// видит пользователь (`publishable`). Аргументы инструмента, если
/// какой-то следующий шаг их использует, берутся из этого же патча через
/// состояние — не из сырого текста модели (mediation.md §4.4, второй
/// пункт); это гарантируется тем, что до `commit` вообще нет пути записи
/// в состояние в обход `parse -> schema`, а не отдельной проверкой здесь.
pub fn commit<C: Contract>(
    step_id: &str,
    contract: &C,
    model_tier: Option<ModelTier>,
) -> CommitOutcome {
    CommitOutcome {
        patch: Patch {
            step_id: step_id.to_string(),
            changes: serde_json::to_value(contract)
                .expect("контракт, прошедший M1/M3, всегда сериализуем"),
        },
        provenance: Provenance {
            contract_name: C::NAME,
            contract_version: C::SCHEMA_VERSION,
            model_tier,
        },
        publishable: contract.publishable(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::{Category, ClassificationOut, SupportReply};

    #[test]
    fn patch_carries_step_id_and_full_contract_as_changes() {
        let contract = ClassificationOut {
            category: Category::Billing,
            risk_factors: vec!["обычный вопрос".into()],
            risk: 4,
            summary: "ok".into(),
        };
        let outcome = commit("classify", &contract, Some(ModelTier::Weak));

        assert_eq!(outcome.patch.step_id, "classify");
        assert_eq!(
            outcome.patch.changes,
            serde_json::json!({"category": "billing", "risk_factors": ["обычный вопрос"], "risk": 4, "summary": "ok"})
        );
    }

    #[test]
    fn provenance_carries_contract_identity_and_model_tier() {
        let contract = ClassificationOut {
            category: Category::Card,
            risk_factors: vec!["x".into()],
            risk: 1,
            summary: "x".into(),
        };
        let outcome = commit("classify", &contract, Some(ModelTier::Strong));

        assert_eq!(outcome.provenance.contract_name, "ClassificationOut");
        // 0.30.0 (issue #4): версия 2 — обязательное risk_factors.
        assert_eq!(outcome.provenance.contract_version, 2);
        assert_eq!(outcome.provenance.model_tier, Some(ModelTier::Strong));
    }

    #[test]
    fn classification_out_publishes_nothing_by_default() {
        let contract = ClassificationOut {
            category: Category::Debt,
            risk_factors: vec!["просрочка".into()],
            risk: 9,
            summary: "просрочка".into(),
        };
        let outcome = commit("classify", &contract, None);
        assert_eq!(
            outcome.publishable,
            Value::Null,
            "внутренняя классификация не должна публиковаться без явного решения контракта"
        );
    }

    #[test]
    fn support_reply_publishes_card_id_and_reply_fields() {
        let contract = SupportReply {
            card_id: "card_1029".into(),
            reply: "Ваш вопрос решён.".into(),
        };
        let outcome = commit("answer", &contract, Some(ModelTier::Medium));
        assert_eq!(
            outcome.publishable,
            serde_json::json!({"card_id": "card_1029", "reply": "Ваш вопрос решён."})
        );
    }

    /// Композиция всей цепочки M1-M2-M3-M5 на реалистичном сыром выводе.
    #[test]
    fn full_pipeline_from_raw_model_text_to_commit() {
        let raw = "```json\n{\"card_id\": \"card_1029\", \"reply\": \"Готово.\"}\n```";
        let parsed = crate::parse::parse(raw).unwrap();
        let contract: SupportReply = crate::schema::validate(parsed).unwrap();
        let outcome = commit("answer", &contract, Some(ModelTier::Weak));

        assert_eq!(outcome.patch.changes["reply"], "Готово.");
        assert_eq!(outcome.publishable["reply"], "Готово.");
    }
}
