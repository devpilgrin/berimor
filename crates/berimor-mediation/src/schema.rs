//! Стадия `schema`: типы, обязательность, перечисления, диапазоны, длины,
//! запрет неизвестных полей.
//!
//! Источник: `docs/arch/mediation.md` §4.2. ROADMAP: M3.
//!
//! Типы, обязательность (не-`Option` поля), перечисления (Rust `enum`) и
//! запрет неизвестных полей (`#[serde(deny_unknown_fields)]`) — уже M1:
//! serde-десериализация либо строит контракт, либо нет, третьего не дано.
//! Здесь — то, что serde сам по себе не проверяет: числовые диапазоны и
//! ограничения длины (`validator::Validate`, объявленные на полях
//! конкретного контракта). Разбито на два явных шага, а не два аспекта
//! одной ошибки — вызывающему коду важно различать «не та форма» (M2
//! должен был это поймать ещё на parse, если бы не валидный JSON) от
//! «форма верна, но значения вне допустимых границ» (разные причины
//! повтора, mediation.md §5: оба — до 2 попыток, но с разным текстом
//! ошибки в подсказке).

use berimor_types::contract::Contract;

#[derive(Debug, thiserror::Error)]
pub enum SchemaError {
    #[error("вывод модели не соответствует форме контракта: {0}")]
    Shape(#[from] serde_json::Error),
    #[error("вывод модели нарушает ограничения контракта: {0}")]
    Constraints(#[from] validator::ValidationErrors),
}

/// Разбирает `value` (уже прошедшее `parse`, M2) в конкретный контракт `C`
/// и проверяет его диапазоны/длины. Отказ на любом из двух шагов — отказ
/// стадии `schema`, ведёт к повтору (M6), не к попытке скорректировать
/// значение самостоятельно.
pub fn validate<C: Contract>(value: serde_json::Value) -> Result<C, SchemaError> {
    let contract: C = serde_json::from_value(value)?;
    validator::Validate::validate(&contract)?;
    Ok(contract)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::{Category, ClassificationOut};
    use crate::parse;
    use serde_json::json;

    #[test]
    fn accepts_value_within_all_constraints() {
        let raw = json!({"category": "billing", "risk": 5, "summary": "ok"});
        let contract: ClassificationOut = validate(raw).unwrap();
        assert_eq!(contract.category, Category::Billing);
    }

    #[test]
    fn accepts_boundary_values_inclusive() {
        let low = json!({"category": "card", "risk": 0, "summary": ""});
        let high = json!({"category": "card", "risk": 10, "summary": "x".repeat(280)});
        assert!(validate::<ClassificationOut>(low).is_ok());
        assert!(validate::<ClassificationOut>(high).is_ok());
    }

    #[test]
    fn rejects_risk_above_documented_maximum() {
        // u8 допускает и 11, и 255 — границу 0..=10 держит только
        // #[validate(range(...))], не сам тип.
        let raw = json!({"category": "card", "risk": 11, "summary": "ok"});
        let result = validate::<ClassificationOut>(raw);
        assert!(matches!(result, Err(SchemaError::Constraints(_))));
    }

    #[test]
    fn rejects_summary_over_max_length() {
        let raw = json!({"category": "card", "risk": 1, "summary": "x".repeat(281)});
        let result = validate::<ClassificationOut>(raw);
        assert!(matches!(result, Err(SchemaError::Constraints(_))));
    }

    #[test]
    fn distinguishes_shape_error_from_constraint_error() {
        // risk отсутствует вовсе — это форма (M1/serde), не диапазон.
        let malformed_shape = json!({"category": "card", "summary": "ok"});
        assert!(matches!(
            validate::<ClassificationOut>(malformed_shape),
            Err(SchemaError::Shape(_))
        ));

        // risk есть, но вне диапазона — это constraints, не форма.
        let bad_constraint = json!({"category": "card", "risk": 99, "summary": "ok"});
        assert!(matches!(
            validate::<ClassificationOut>(bad_constraint),
            Err(SchemaError::Constraints(_))
        ));
    }

    /// Композиция всей цепочки, построенной до сих пор: сырой текст
    /// модели (возможно, в markdown-обёртке) -> parse (M2) -> validate (M3).
    #[test]
    fn composes_with_parse_stage() {
        let raw = "```json\n{\"category\": \"debt\", \"risk\": 8, \"summary\": \"просрочка\"}\n```";
        let parsed = parse::parse(raw).unwrap();
        let contract: ClassificationOut = validate(parsed).unwrap();
        assert_eq!(contract.risk, 8);
    }
}
