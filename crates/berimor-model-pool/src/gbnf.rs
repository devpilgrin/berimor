//! Конвертер JSON Schema → GBNF (грамматика llama.cpp) — жёсткое
//! принуждение структуры и ПОРЯДКА полей для локальных моделей
//! (SGR-волна 0.30.x, issue #3: «grammar» для автономного llama.cpp).
//!
//! Осознанное подмножество схем (контракты беримора — плоские/вложенные
//! объекты с примитивами и массивами):
//! - object: ВСЕ `properties` обязаны быть в `required` — иначе
//!   конвертер отказывает (опциональные поля в GBNF требуют
//!   альтернатив-перестановок, цена не оправдана: наши контракты
//!   объявляют все поля обязательными). Порядок правил = порядок
//!   `properties` (schemars `preserve_order`) — генерация идёт
//!   физически в порядке объявления, это и есть принуждение SGR;
//! - string: произвольная или `enum` (альтернативы литералов);
//! - integer/number/boolean/null — стандартные лексемы;
//! - array: `items` (подмножество выше), `minItems` 0 → тело опционально;
//! - anyOf/oneOf/$ref/pattern — отказ (`Err`): вызывающий откатывается
//!   на промпт-уровень. Отказ честный, не молчаливая деградация
//!   грамматики.

use serde_json::Value;

/// Конвертирует JSON Schema контракта в GBNF-грамматику с корнем `root`.
/// Err — схема вне поддерживаемого подмножества (причина в тексте).
pub fn json_schema_to_gbnf(schema: &Value) -> Result<String, String> {
    let mut rules = Vec::new();
    emit_rule("root", schema, &mut rules)?;
    // Общие лексемы JSON (ws, string, number...) — один раз.
    rules.push(r#"ws ::= [ \t\n]*"#.to_string());
    rules.push(
        r#"string-literal ::= "\"" ([^"\\\x7F\x00-\x1F] | "\\" (["\\/bfnrt] | "u" [0-9a-fA-F]{4}))* "\""#
            .to_string(),
    );
    rules.push(r#"integer-value ::= "-"? [0-9]+"#.to_string());
    rules.push(r#"number-value ::= "-"? [0-9]+ ("." [0-9]+)? ([eE] [-+]? [0-9]+)?"#.to_string());
    Ok(rules.join("\n") + "\n")
}

/// Одно правило GBNF для узла схемы; вложенные объекты/массивы
/// материализуются в именованные правила (имя — по пути полей).
fn emit_rule(name: &str, schema: &Value, rules: &mut Vec<String>) -> Result<String, String> {
    let body = emit_body(name, schema, rules)?;
    rules.push(format!("{name} ::= {body}"));
    Ok(name.to_string())
}

fn emit_body(name: &str, schema: &Value, rules: &mut Vec<String>) -> Result<String, String> {
    let ty = schema.get("type").and_then(Value::as_str);
    match ty {
        Some("object") => emit_object(name, schema, rules),
        Some("string") => {
            if let Some(variants) = schema.get("enum").and_then(Value::as_array) {
                let mut alts = Vec::new();
                for variant in variants {
                    let text = variant
                        .as_str()
                        .ok_or("enum со значением не-строкой — вне подмножества")?;
                    alts.push(gbnf_string_literal(text));
                }
                Ok(format!("({}) ws", alts.join(" | ")))
            } else {
                Ok("string-literal ws".into())
            }
        }
        Some("integer") => Ok("integer-value ws".into()),
        Some("number") => Ok("number-value ws".into()),
        Some("boolean") => Ok("(\"true\" | \"false\") ws".into()),
        Some("null") => Ok("\"null\" ws".into()),
        Some("array") => emit_array(name, schema, rules),
        Some(other) => Err(format!("тип '{other}' — вне подмножества GBNF-конвертера")),
        None if schema.get("properties").is_some() => emit_object(name, schema, rules),
        None => Err("узел схемы без 'type' — вне подмножества GBNF-конвертера".into()),
    }
}

fn emit_object(name: &str, schema: &Value, rules: &mut Vec<String>) -> Result<String, String> {
    let properties = schema
        .get("properties")
        .and_then(Value::as_object)
        .ok_or("object без 'properties' — вне подмножества")?;
    let required: Vec<&str> = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    // Осознанное ограничение: опциональные поля — отказ (см. шапку).
    for field in properties.keys() {
        if !required.contains(&field.as_str()) {
            return Err(format!(
                "поле '{field}' не в required — опциональные поля вне подмножества GBNF-конвертера"
            ));
        }
    }
    let mut parts = vec![r#""{" ws"#.to_string()];
    for (index, (field, field_schema)) in properties.iter().enumerate() {
        if index > 0 {
            parts.push(r#""," ws"#.into());
        }
        parts.push(gbnf_string_literal(field));
        parts.push(r#"":" ws"#.into());
        let rule_name = format!("{name}-{}", sanitize(field));
        parts.push(emit_rule(&rule_name, field_schema, rules)?);
    }
    parts.push(r#""}" ws"#.into());
    Ok(parts.join(" "))
}

fn emit_array(name: &str, schema: &Value, rules: &mut Vec<String>) -> Result<String, String> {
    let items = schema
        .get("items")
        .ok_or("array без 'items' — вне подмножества")?;
    let item_rule = emit_rule(&format!("{name}-item", name = name), items, rules)?;
    let min_items = schema.get("minItems").and_then(Value::as_u64).unwrap_or(0);
    let body = if min_items == 0 {
        format!(r#""[" ws ({item_rule} ("," ws {item_rule})*)? "]" ws"#)
    } else {
        format!(r#""[" ws {item_rule} ("," ws {item_rule})* "]" ws"#)
    };
    Ok(body)
}

/// Имя правила из имени поля: GBNF допускает буквы/цифры/дефис.
fn sanitize(field: &str) -> String {
    field
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// Строковый литерал внутри GBNF-правила: экранирование кавычек и
/// обратного слэша; не-ASCII (кириллица имён полей) — UTF-8 как есть,
/// llama.cpp принимает байтовые литералы.
fn gbnf_string_literal(text: &str) -> String {
    let escaped = text.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Контракт вида ClassificationOut v2: поля-обоснования перед целевыми.
    #[test]
    fn converts_flat_object_preserving_property_order() {
        let schema = json!({
            "type": "object",
            "properties": {
                "category": {"type": "string", "enum": ["card", "other"]},
                "risk_factors": {"type": "array", "items": {"type": "string"}, "minItems": 1},
                "risk": {"type": "integer"},
                "summary": {"type": "string"}
            },
            "required": ["category", "risk_factors", "risk", "summary"]
        });
        let gbnf = json_schema_to_gbnf(&schema).expect("плоский объект конвертируется");
        // Порядок полей в правиле root = порядок объявления (SGR).
        let root = gbnf.lines().find(|l| l.starts_with("root ::=")).unwrap();
        let i_cat = root.find("\"category\"").unwrap();
        let i_factors = root.find("\"risk_factors\"").unwrap();
        let i_risk = root.find("\"risk\"").unwrap();
        assert!(i_cat < i_factors && i_factors < i_risk, "порядок: {root}");
        assert!(root.contains(r#""{" ws"#), "объект открывается");
        assert!(gbnf.contains("root-category ::= (\"card\" | \"other\") ws"));
        assert!(gbnf.contains("root-risk-factors ::= \"[\" ws root-risk-factors-item (\",\" ws root-risk-factors-item)* \"]\" ws"));
        assert!(gbnf.contains("root-risk ::= integer-value ws"));
    }

    #[test]
    fn nested_object_materializes_named_rules() {
        let schema = json!({
            "type": "object",
            "properties": {
                "user": {"type": "object",
                         "properties": {"id": {"type": "string"}},
                         "required": ["id"]}
            },
            "required": ["user"]
        });
        let gbnf = json_schema_to_gbnf(&schema).unwrap();
        assert!(
            gbnf.contains("root-user ::= \"{\" ws \"id\" \":\" ws root-user-id \"}\" ws"),
            "{gbnf}"
        );
        assert!(
            gbnf.contains("root-user-id ::= string-literal ws"),
            "{gbnf}"
        );
    }

    #[test]
    fn optional_field_is_refused_honestly() {
        let schema = json!({
            "type": "object",
            "properties": {"a": {"type": "string"}},
            "required": []
        });
        let err = json_schema_to_gbnf(&schema).unwrap_err();
        assert!(err.contains("required"), "{err}");
    }

    #[test]
    fn anyof_is_refused_honestly() {
        let schema = json!({"anyOf": [{"type": "string"}, {"type": "integer"}]});
        assert!(json_schema_to_gbnf(&schema).is_err());
    }

    #[test]
    fn field_names_with_underscores_sanitize_to_rule_names() {
        let schema = json!({
            "type": "object",
            "properties": {"card_id": {"type": "string"}},
            "required": ["card_id"]
        });
        let gbnf = json_schema_to_gbnf(&schema).unwrap();
        assert!(gbnf.contains("root-card-id ::="), "{gbnf}");
        assert!(
            gbnf.contains(r#""card_id""#),
            "литерал поля не искажён: {gbnf}"
        );
    }
}
