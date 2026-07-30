//! Резолвинг путей вида `state.classify.risk` в значении состояния.
//!
//! Источник: `docs/arch/process-engine.md` §2 (шаблоны в декларации
//! процесса — `on: state.classify.risk`, `args: {id: "{{state.user.card_id}}"}}`).
//! Используется и `graph::evaluate_branch` (P2), и `executors::tool_only`
//! (E1) — вынесено сюда, когда второй потребитель сделал дублирование
//! неоправданным (обе крайности — копия в каждом крейте и преждевременная
//! абстракция раньше времени — были бы хуже одной маленькой общей функции).

use serde_json::Value;

/// Ведущий `state.` — часть синтаксиса шаблонов, сам объект `state`,
/// который сюда передаётся, уже и есть то дерево, на которое ссылается путь.
pub fn resolve<'a>(path: &str, state: &'a Value) -> Option<&'a Value> {
    let path = path.strip_prefix("state.").unwrap_or(path);
    let mut current = state;
    for segment in path.split('.') {
        current = current.as_object()?.get(segment)?;
    }
    Some(current)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn resolves_nested_path_with_state_prefix() {
        let state = json!({"classify": {"risk": 7}});
        assert_eq!(resolve("state.classify.risk", &state), Some(&json!(7)));
    }

    #[test]
    fn resolves_path_without_state_prefix() {
        let state = json!({"classify": {"risk": 7}});
        assert_eq!(resolve("classify.risk", &state), Some(&json!(7)));
    }

    #[test]
    fn missing_path_is_none_not_a_panic() {
        let state = json!({"classify": {"risk": 7}});
        assert_eq!(resolve("state.classify.nonexistent", &state), None);
    }

    #[test]
    fn non_object_intermediate_is_none_not_a_panic() {
        let state = json!({"classify": 5});
        assert_eq!(resolve("state.classify.risk", &state), None);
    }
}
