//! Разбор человеко-читаемых значений в декларации процесса.
//!
//! Источник: `docs/arch/process-engine.md` §2 — пример декларации пишет
//! `timeout: 10m` и `token_budget: 100k`, не голые числа. ROADMAP: P1.
//!
//! Живёт в `berimor-types`, а не в `berimor-process-engine`, где
//! содержательно находится парсер (P1): `#[derive(Deserialize)]` на
//! `ProcessLimits` раскрывается здесь же, и `deserialize_with` обязан
//! указывать на функцию, видимую в этой точке — крейт с типом не может
//! зависеть от крейта, который зависит от него самого.

use serde::Deserialize;

/// `"10m"` → 600, `"30s"` → 30, `"1h"` → 3600. Суффикс обязателен —
/// голое число без единицы измерения не значит ничего однозначного и
/// отклоняется, а не молча трактуется как секунды или как ошибка формата,
/// которую легко не заметить.
pub fn parse_duration_seconds(input: &str) -> Result<u64, String> {
    let s = input.trim();
    let split_at = s
        .find(|c: char| !c.is_ascii_digit())
        .ok_or_else(|| format!("нет единицы измерения времени в '{s}' (ожидались s/m/h)"))?;
    let (number, unit) = s.split_at(split_at);
    let value: u64 = number
        .parse()
        .map_err(|_| format!("не число перед единицей измерения в '{s}'"))?;

    let multiplier: u64 = match unit {
        "s" => 1,
        "m" => 60,
        "h" => 3_600,
        other => {
            return Err(format!(
                "неизвестная единица измерения времени '{other}' в '{s}'"
            ))
        }
    };
    Ok(value * multiplier)
}

/// `"100k"` → 100_000, `"2m"` → 2_000_000, голое число — как есть.
/// Суффиксы `k`/`m` здесь про количество (тысяча/миллион), не про время —
/// в отличие от [`parse_duration_seconds`], где `m` значит минуты; это два
/// разных поля с разными единицами, путать их местами негде.
pub fn parse_count(input: &str) -> Result<u64, String> {
    let s = input.trim();
    if let Some(base) = s.strip_suffix('k').or_else(|| s.strip_suffix('K')) {
        return scale(base, 1_000.0, s);
    }
    if let Some(base) = s.strip_suffix('m').or_else(|| s.strip_suffix('M')) {
        return scale(base, 1_000_000.0, s);
    }
    s.parse().map_err(|_| format!("не число в '{s}'"))
}

fn scale(base: &str, factor: f64, original: &str) -> Result<u64, String> {
    let value: f64 = base
        .parse()
        .map_err(|_| format!("не число перед суффиксом в '{original}'"))?;
    Ok((value * factor).round() as u64)
}

/// YAML/JSON различают, была ли исходная величина числом или строкой —
/// `100000` и `"100k"` должны разбираться одинаково успешно.
#[derive(Deserialize)]
#[serde(untagged)]
enum NumberOrText {
    Number(u64),
    Text(String),
}

pub fn deserialize_duration_seconds<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    match NumberOrText::deserialize(deserializer)? {
        NumberOrText::Number(n) => Ok(n),
        NumberOrText::Text(s) => parse_duration_seconds(&s).map_err(serde::de::Error::custom),
    }
}

/// Для необязательных полей вроде `human_gate.timeout` (P7) — та же
/// разметка суффиксов, что [`deserialize_duration_seconds`], но поле
/// может отсутствовать вовсе (ждать без таймаута — текущее поведение
/// Milestone 0/1, `#[serde(default)]` сохраняет обратную совместимость с
/// golden-фикстурой, где `human_gate` таймаута не объявляет).
pub fn deserialize_optional_duration_seconds<'de, D>(
    deserializer: D,
) -> Result<Option<u64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    match Option::<NumberOrText>::deserialize(deserializer)? {
        None => Ok(None),
        Some(NumberOrText::Number(n)) => Ok(Some(n)),
        Some(NumberOrText::Text(s)) => parse_duration_seconds(&s)
            .map(Some)
            .map_err(serde::de::Error::custom),
    }
}

pub fn deserialize_optional_count<'de, D>(deserializer: D) -> Result<Option<u64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    match Option::<NumberOrText>::deserialize(deserializer)? {
        None => Ok(None),
        Some(NumberOrText::Number(n)) => Ok(Some(n)),
        Some(NumberOrText::Text(s)) => parse_count(&s).map(Some).map_err(serde::de::Error::custom),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duration_suffixes() {
        assert_eq!(parse_duration_seconds("30s"), Ok(30));
        assert_eq!(parse_duration_seconds("10m"), Ok(600));
        assert_eq!(parse_duration_seconds("2h"), Ok(7200));
    }

    #[test]
    fn duration_rejects_missing_unit() {
        assert!(parse_duration_seconds("30").is_err());
    }

    #[test]
    fn duration_rejects_unknown_unit() {
        assert!(parse_duration_seconds("30d").is_err());
    }

    #[test]
    fn count_suffixes() {
        assert_eq!(parse_count("100k"), Ok(100_000));
        assert_eq!(parse_count("1.5k"), Ok(1_500));
        assert_eq!(parse_count("2m"), Ok(2_000_000));
        assert_eq!(parse_count("42"), Ok(42));
    }

    #[test]
    fn count_rejects_garbage() {
        assert!(parse_count("abc").is_err());
    }

    #[derive(Deserialize)]
    struct OptionalDurationHolder {
        #[serde(default, deserialize_with = "deserialize_optional_duration_seconds")]
        timeout: Option<u64>,
    }

    #[test]
    fn optional_duration_absent_field_is_none() {
        let parsed: OptionalDurationHolder = serde_json::from_str("{}").unwrap();
        assert_eq!(parsed.timeout, None);
    }

    #[test]
    fn optional_duration_parses_suffixed_string() {
        let parsed: OptionalDurationHolder = serde_json::from_str(r#"{"timeout": "10m"}"#).unwrap();
        assert_eq!(parsed.timeout, Some(600));
    }

    #[test]
    fn optional_duration_parses_bare_number_as_seconds() {
        let parsed: OptionalDurationHolder = serde_json::from_str(r#"{"timeout": 30}"#).unwrap();
        assert_eq!(parsed.timeout, Some(30));
    }

    #[test]
    fn optional_duration_rejects_unknown_unit() {
        let result: Result<OptionalDurationHolder, _> =
            serde_json::from_str(r#"{"timeout": "30d"}"#);
        assert!(result.is_err());
    }
}
