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

/// Достройка оборванного JSON (полевой репорт 2026-08-16: локальные
/// модели утыкаются в потолок генерации — «EOF while parsing an object»
/// на десятках килобайт — и процесс умирал эскалацией после 3 ретраев).
/// Это НЕ угадывание содержимого (табу из шапки): достраивается только
/// СТРУКТУРА — закрывающие кавычки/скобки по таблице суффиксов, первое
/// парсящееся — принимается. Содержимое не меняется. Каждый ремонт
/// виден в трассе медиации (MediationParseRepaired).
pub fn repair_truncated_json(candidate: &str) -> Option<serde_json::Value> {
    // Кандидаты: сначала закрыть незакрытую строку, затем скобки;
    // порядок — от меньшего вмешательства к большему.
    const SUFFIXES: &[&str] = &[
        "\"", "\"}", "}", "\"]}", "]}", "}", "\"}]}", "}]}", "}}", "]}", "\"]", "]",
    ];
    for suffix in SUFFIXES {
        let repaired = format!("{candidate}{suffix}");
        if let Ok(value) = serde_json::from_str(&repaired) {
            return Some(value);
        }
    }
    None
}

/// Разбор с достройкой обрыва. Возвращает (значение, был_ремонт).
pub fn parse_with_repair(raw: &str) -> Result<(serde_json::Value, bool), ParseError> {
    let candidate = strip_markdown_fence(raw);
    match serde_json::from_str(candidate) {
        Ok(value) => Ok((value, false)),
        Err(source) => {
            // Достройка — только для EOF-класса (обрыв генерации);
            // прочие ошибки (мусор, не-JSON) — честный отказ.
            if source.to_string().contains("EOF while parsing") {
                if let Some(value) = repair_truncated_json(candidate) {
                    return Ok((value, true));
                }
            }
            Err(ParseError { source })
        }
    }
}

/// Разбирает сырой вывод модели в `serde_json::Value`. Снимает ровно один
/// слой markdown-обёртки на границах текста, если она есть; содержимое
/// внутри — либо валидный JSON, либо отказ.
pub fn parse(raw: &str) -> Result<serde_json::Value, ParseError> {
    parse_with_repair(raw).map(|(value, _)| value)
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

    /// EOF-ремонт (0.35.2, репорт о локальных моделях): обрыв генерации
    /// достраивается структурно; ремонт помечается флагом.
    #[test]
    fn truncated_output_is_repaired_and_flagged() {
        // Обрыв внутри строки.
        let (value, repaired) =
            parse_with_repair(r#"{"summary": "клиент жалуется на задерж"#).unwrap();
        assert!(repaired);
        assert_eq!(value["summary"], "клиент жалуется на задерж");
        // Обрыв после значения, объект не закрыт.
        let (value, repaired) = parse_with_repair(r#"{"risk": 8"#).unwrap();
        assert!(repaired);
        assert_eq!(value["risk"], json!(8));
        // Обрыв внутри массива внутри объекта.
        let (value, repaired) =
            parse_with_repair(r#"{"risk_factors": ["просрочка", "угроза ЦБ""#).unwrap();
        assert!(repaired);
        assert_eq!(value["risk_factors"], json!(["просрочка", "угроза ЦБ"]));
        // Мусор (не EOF) — честный отказ без ремонта.
        assert!(parse_with_repair("это не json вовсе").is_err());
        assert!(parse_with_repair(r#"{"a": }"#).is_err());
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
        // Незакрытая обёртка с НЕ-JSON содержимым — отказ (EOF-ремонт
        // достраивает структуру оборванного JSON, не спасает мусор).
        let raw = "```json\nне json вовсе";
        assert!(parse(raw).is_err());
        // А оборванный валидный JSON в незакрытой обёртке — достраивается
        // (0.35.2: обрыв генерации локальной модели выглядит ровно так).
        let raw = "```json\n{\"a\": 1";
        assert_eq!(parse(raw).unwrap(), json!({"a": 1}));
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
