//! ToolOnly — шаг без семантики: чтение/запись/вызов внешней системы, без модели.
//!
//! Источник: `docs/arch/executors.md` §2. ROADMAP: E1.
//!
//! Milestone 0 (`docs/ROADMAP.md` §3) допускает E1 без S1–S4 (Capability):
//! реального deny-статики/jail/сетевого гейта/режимов подтверждения здесь
//! нет — `ToolDispatch` вызывается напрямую. Полноценный `ToolOnly` из
//! `executors.md` §2 («перед вызовом — capability-слой») подключит
//! `berimor-capability` отдельной задачей, когда она появится; здесь —
//! только резолвинг шаблонов и диспетч, честно ограниченные этим.

use berimor_types::step::Patch;
use serde_json::Value;

/// Единственная точка выхода наружу — реализация подключает конкретную
/// внешнюю систему (в тестах — фейк, в реальном использовании — то, что
/// сверх этого модуля: HTTP-клиент, CLI-обёртка и т.д.).
pub trait ToolDispatch {
    fn call(&self, tool: &str, args: &Value) -> Result<Value, DispatchError>;
}

#[derive(Debug, thiserror::Error)]
#[error("вызов инструмента '{tool}' завершился ошибкой: {reason}")]
pub struct DispatchError {
    pub tool: String,
    pub reason: String,
}

#[derive(Debug, thiserror::Error)]
pub enum ToolOnlyError {
    #[error("не удалось разрешить шаблон: путь '{0}' не найден в состоянии")]
    UnresolvedTemplate(String),
    #[error(transparent)]
    Dispatch(#[from] DispatchError),
}

/// Резолвит шаблоны аргументов из состояния и вызывает инструмент.
/// Результат становится `changes` патча как есть — `ToolOnly` не требует
/// контракта (`executors.md` §2: «инструмент — доверенный код, контракт
/// не нужен»), в отличие от шагов с моделью.
pub fn execute(
    step_id: &str,
    tool: &str,
    args_template: &Value,
    state: &Value,
    dispatch: &dyn ToolDispatch,
) -> Result<Patch, ToolOnlyError> {
    let resolved_args = resolve_template(args_template, state)?;
    let result = dispatch.call(tool, &resolved_args)?;
    Ok(Patch {
        step_id: step_id.to_string(),
        changes: result,
    })
}

/// Разрешает шаблоны вида `{{state.a.b}}` рекурсивно по объекту/массиву.
/// Строковое значение — плейсхолдер, только если ИМ ЦЕЛИКОМ является
/// (`"{{state.user.card_id}}"` из golden-фикстуры, не подстрока внутри
/// более длинного текста) — узкая, детерминированная толерантность в
/// духе `mediation.md` §4.1, не попытка угадать частичную интерполяцию,
/// для которой нет примера ни в одном документе.
fn resolve_template(template: &Value, state: &Value) -> Result<Value, ToolOnlyError> {
    match template {
        Value::String(s) => match extract_placeholder(s) {
            Some(path) => berimor_types::state_path::resolve(path, state)
                .cloned()
                .ok_or_else(|| ToolOnlyError::UnresolvedTemplate(path.to_string())),
            None => Ok(template.clone()),
        },
        Value::Object(map) => {
            let mut resolved = serde_json::Map::with_capacity(map.len());
            for (key, value) in map {
                resolved.insert(key.clone(), resolve_template(value, state)?);
            }
            Ok(Value::Object(resolved))
        }
        Value::Array(items) => {
            let resolved: Result<Vec<_>, _> = items
                .iter()
                .map(|item| resolve_template(item, state))
                .collect();
            Ok(Value::Array(resolved?))
        }
        other => Ok(other.clone()),
    }
}

fn extract_placeholder(s: &str) -> Option<&str> {
    s.strip_prefix("{{")?.strip_suffix("}}").map(str::trim)
}

#[cfg(test)]
mod tests {
    use super::*;
    use berimor_types::step::StepKind;
    use serde_json::json;

    struct FakeCrm;

    impl ToolDispatch for FakeCrm {
        fn call(&self, tool: &str, args: &Value) -> Result<Value, DispatchError> {
            match tool {
                "crm.get_card_status" => Ok(json!({"status": "active", "card_id": args["id"]})),
                other => Err(DispatchError {
                    tool: other.into(),
                    reason: "неизвестный инструмент в фейке теста".into(),
                }),
            }
        }
    }

    struct AlwaysFails;
    impl ToolDispatch for AlwaysFails {
        fn call(&self, tool: &str, _args: &Value) -> Result<Value, DispatchError> {
            Err(DispatchError {
                tool: tool.into(),
                reason: "намеренный сбой теста".into(),
            })
        }
    }

    #[test]
    fn resolves_simple_placeholder() {
        let template = json!({"id": "{{state.user.card_id}}"});
        let state = json!({"user": {"card_id": "c-1"}});
        let resolved = resolve_template(&template, &state).unwrap();
        assert_eq!(resolved, json!({"id": "c-1"}));
    }

    #[test]
    fn non_placeholder_strings_pass_through_unchanged() {
        let template = json!({"note": "не шаблон"});
        let resolved = resolve_template(&template, &json!({})).unwrap();
        assert_eq!(resolved, json!({"note": "не шаблон"}));
    }

    #[test]
    fn missing_state_path_is_an_error_not_null() {
        let template = json!({"id": "{{state.user.card_id}}"});
        let result = resolve_template(&template, &json!({}));
        assert!(matches!(result, Err(ToolOnlyError::UnresolvedTemplate(_))));
    }

    #[test]
    fn resolves_nested_objects_and_arrays() {
        let template = json!({
            "filters": [{"key": "id", "value": "{{state.user.card_id}}"}]
        });
        let state = json!({"user": {"card_id": "c-1"}});
        let resolved = resolve_template(&template, &state).unwrap();
        assert_eq!(resolved["filters"][0]["value"], "c-1");
    }

    #[test]
    fn execute_produces_patch_from_dispatch_result() {
        let state = json!({"user": {"card_id": "c-1"}});
        let patch = execute(
            "fetch_card_status",
            "crm.get_card_status",
            &json!({"id": "{{state.user.card_id}}"}),
            &state,
            &FakeCrm,
        )
        .unwrap();

        assert_eq!(patch.step_id, "fetch_card_status");
        assert_eq!(patch.changes["status"], "active");
        assert_eq!(patch.changes["card_id"], "c-1");
    }

    #[test]
    fn dispatch_failure_propagates_not_swallowed() {
        let result = execute(
            "fetch_card_status",
            "crm.get_card_status",
            &json!({"id": "{{state.user.card_id}}"}),
            &json!({"user": {"card_id": "c-1"}}),
            &AlwaysFails,
        );
        assert!(matches!(result, Err(ToolOnlyError::Dispatch(_))));
    }

    /// Композиция с P1: аргументы шага `fetch_card_status`, как их реально
    /// разобрал парсер golden-фикстуры, резолвятся и уходят в диспетч.
    #[test]
    fn composes_with_parsed_golden_fixture_step() {
        const GOLDEN_FIXTURE: &str =
            include_str!("../../../fixtures/golden/processes/card-delivery-support.yaml");
        let process = berimor_process_engine::parser::parse(GOLDEN_FIXTURE).unwrap();
        let step = process
            .steps
            .iter()
            .find(|s| s.id == "fetch_card_status")
            .unwrap();

        let StepKind::Tool { tool, args } = &step.kind else {
            panic!("ожидался Tool");
        };

        let state = json!({"user": {"card_id": "c-42"}});
        let patch = execute(&step.id, tool, args, &state, &FakeCrm).unwrap();

        assert_eq!(patch.changes["card_id"], "c-42");
    }
}
