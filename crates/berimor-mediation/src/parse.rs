//! Стадия `parse`: извлечение данных из сырого вывода модели.
//!
//! Источник: `docs/arch/mediation.md` §4.1. ROADMAP: M2.
//!
//! Толерантность — узкая и специфицированная: только оформительские
//! markdown-обёртки (```` ```json ... ``` ````), распространённая
//! привычка моделей заворачивать структурированный ответ в блок кода.
//! Никакой эвристики поверх содержимого — «эвристический разбор — скрытый
//! источник невоспроизводимости» (буквально из документа). Если после
//! снятия обёртки текст не парсится как JSON — это отказ стадии `parse`,
//! ведущий к повтору (M6), а не попытка угадать, что имелось в виду.

#[derive(Debug, thiserror::Error)]
#[error("не удалось разобрать вывод модели как JSON: {source}")]
pub struct ParseError {
    #[source]
    pub source: serde_json::Error,
}

/// Разбирает сырой вывод модели в `serde_json::Value`. Снимает ровно один
/// слой markdown-обёртки на границах текста, если она есть; содержимое
/// внутри — либо валидный JSON, либо отказ.
pub fn parse(raw: &str) -> Result<serde_json::Value, ParseError> {
    let candidate = strip_markdown_fence(raw);
    serde_json::from_str(candidate).map_err(|source| ParseError { source })
}

/// Снимает markdown code fence (```` ``` ```` или ```` ```json ````) с
/// краёв текста, если он им обёрнут целиком. Проверяет только начало и
/// конец — вхождение `` ``` `` где-то в середине (например, внутри
/// строкового значения JSON) не трогает.
fn strip_markdown_fence(raw: &str) -> &str {
    let trimmed = raw.trim();
    let Some(without_open) = trimmed.strip_prefix("```") else {
        return trimmed;
    };
    let without_close = without_open.strip_suffix("```").unwrap_or(without_open);

    match without_close.split_once('\n') {
        Some((first_line, rest)) if is_language_tag(first_line) => rest.trim(),
        _ => without_close.trim(),
    }
}

/// Первая строка внутри обёртки — языковой тег (`json`, `yaml`, ...), а не
/// часть содержимого, если состоит только из букв/цифр и не пуста.
fn is_language_tag(line: &str) -> bool {
    !line.is_empty() && line.chars().all(|c| c.is_ascii_alphanumeric())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::ClassificationOut;
    use serde_json::json;

    #[test]
    fn parses_bare_json_without_fence() {
        let result = parse(r#"{"a": 1}"#).unwrap();
        assert_eq!(result, json!({"a": 1}));
    }

    #[test]
    fn strips_fence_with_json_language_tag() {
        let raw = "```json\n{\"a\": 1}\n```";
        assert_eq!(parse(raw).unwrap(), json!({"a": 1}));
    }

    #[test]
    fn strips_fence_without_language_tag() {
        let raw = "```\n{\"a\": 1}\n```";
        assert_eq!(parse(raw).unwrap(), json!({"a": 1}));
    }

    #[test]
    fn tolerates_surrounding_whitespace() {
        let raw = "\n\n   ```json\n{\"a\": 1}\n```   \n";
        assert_eq!(parse(raw).unwrap(), json!({"a": 1}));
    }

    #[test]
    fn rejects_malformed_json_inside_fence_instead_of_guessing() {
        let raw = "```json\n{\"a\": 1,}\n```"; // висячая запятая
        assert!(parse(raw).is_err());
    }

    #[test]
    fn rejects_prose_around_json_no_heuristic_extraction() {
        // Модель могла бы написать "Вот результат: {...}" — parse не
        // пытается вычленить JSON из произвольного текста, это и была бы
        // запрещённая эвристика.
        let raw = "Вот результат: {\"a\": 1}";
        assert!(
            parse(raw).is_err(),
            "не заворачивающий текст вокруг JSON не должен угадываться"
        );
    }

    #[test]
    fn fence_marker_inside_string_value_is_not_treated_as_wrapper() {
        let raw = r#"{"summary": "используйте ```code``` для примеров"}"#;
        let result = parse(raw).unwrap();
        assert_eq!(result["summary"], "используйте ```code``` для примеров");
    }

    #[test]
    fn unterminated_fence_fails_naturally_without_special_casing() {
        let raw = "```json\n{\"a\": 1"; // нет закрывающей обёртки
        assert!(parse(raw).is_err());
    }

    /// Композиция с M1: то, что вернул parse, обязано ложиться в реальный
    /// контракт без дополнительной подгонки.
    #[test]
    fn parsed_output_deserializes_into_a_real_contract() {
        let raw = "```json\n{\"category\": \"billing\", \"risk_factors\": [\"разовое списание\"], \"risk\": 2, \"summary\": \"ok\"}\n```";
        let value = parse(raw).unwrap();
        let contract: ClassificationOut = serde_json::from_value(value).unwrap();
        assert_eq!(contract.risk, 2);
    }
}
