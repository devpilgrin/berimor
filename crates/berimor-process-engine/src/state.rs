//! Иммутабельное состояние: атомарное применение патча, свёртка журнала.
//!
//! Источник: `docs/arch/process-engine.md` §3 («Состояние»), §7 (свойство
//! «свёртка журнала событий равна живому состоянию»). ROADMAP: F2.
//!
//! Обе функции чистые и непроверяющие: они не валидируют содержимое
//! `changes` — к моменту, когда патч сюда попадает, он уже прошёл
//! Mediation (`mediation.md` §1: «ни один шаг с моделью не пишет в
//! состояние напрямую»). Атомарность здесь — про целостность перехода
//! состояния (нет промежуточных полу-применённых значений), не про
//! проверку смысла.

use berimor_types::event::{Event, EventKind};
use berimor_types::step::Patch;
use serde_json::{Map, Value};

/// Атомарно применяет патч к состоянию: `state[patch.step_id] = patch.changes`.
///
/// Соглашение — не произвольный выбор: пример процесса в `process-engine.md`
/// §2 обращается к полям через `state.classify.risk`, `state.user.card_id` —
/// то есть состояние всегда объект, ключи которого — идентификаторы шагов
/// (плюс начальный ввод), а значение под ключом — последний применённый
/// патч этого шага целиком, не глубокое слияние с предыдущим.
///
/// Не мутирует `state` — возвращает новое значение. Исходный `Value`
/// остаётся валидным и неизменным после вызова (проверено тестом).
pub fn apply_patch(state: &Value, patch: &Patch) -> Value {
    let mut map = match state {
        Value::Object(map) => map.clone(),
        _ => Map::new(),
    };
    map.insert(patch.step_id.clone(), patch.changes.clone());
    Value::Object(map)
}

/// Атомарно применяет патч ОДНОЙ ветви parallel-шага в изолированный
/// неймспейс `state.parallel.<fork_step_id>.<branch_step_id>` (P5,
/// `process-engine.md` §4: «параллельные шаги пишут в разделённые
/// неймспейсы»). Единственное осознанное исключение из «не глубокое
/// слияние» у [`apply_patch`]: здесь ровно один дополнительный уровень
/// вложенности (`parallel.<fork>.<branch>`), не общий deep-merge всего
/// состояния — остальные ключи `parallel.<fork>.*` (другие ветви того же
/// forка, уже завершённые) сохраняются, остальные ключи состояния вне
/// `parallel` не трогаются вовсе.
pub fn apply_parallel_patch(
    state: &Value,
    fork_step_id: &str,
    branch_step_id: &str,
    changes: &Value,
) -> Value {
    let mut root = match state {
        Value::Object(map) => map.clone(),
        _ => Map::new(),
    };
    let mut parallel = match root.remove("parallel") {
        Some(Value::Object(map)) => map,
        _ => Map::new(),
    };
    let mut fork = match parallel.remove(fork_step_id) {
        Some(Value::Object(map)) => map,
        _ => Map::new(),
    };
    fork.insert(branch_step_id.to_string(), changes.clone());
    parallel.insert(fork_step_id.to_string(), Value::Object(fork));
    root.insert("parallel".to_string(), Value::Object(parallel));
    Value::Object(root)
}

/// Свёртка журнала событий инстанса в состояние — детерминированно, без
/// моделей (инвариант I7). Мутируют состояние два вида событий:
/// `Instantiated` (единожды, первым — сидирует состояние исходным `input`
/// на верхнем уровне, не под отдельным ключом: без этого `state.user.card_id`
/// из шаблонов был бы недостижим после восстановления) и `StepApplied`.
/// Остальные виды (`MediationRejected`, `HumanGateOpened` и т.д.) —
/// аудит-след, не патчи (`process-engine.md` §3: «каждый патч — событие
/// `step.applied`»).
///
/// Порядок важен и не проверяется здесь — вызывающий код обязан передавать
/// события, отсортированные по `seq` (так их и возвращает
/// `EventLog::replay` из `berimor-storage`).
pub fn fold(events: &[Event]) -> Value {
    let mut state = Value::Object(Map::new());
    for event in events {
        match &event.kind {
            EventKind::Instantiated => {
                if let (Value::Object(base), Value::Object(input)) = (&mut state, &event.payload) {
                    for (key, value) in input {
                        base.insert(key.clone(), value.clone());
                    }
                }
            }
            EventKind::StepApplied { step_id } => {
                let patch = Patch {
                    step_id: step_id.clone(),
                    changes: event.payload.clone(),
                };
                state = apply_patch(&state, &patch);
            }
            EventKind::ParallelStepApplied {
                fork_step_id,
                branch_step_id,
            } => {
                state = apply_parallel_patch(&state, fork_step_id, branch_step_id, &event.payload);
            }
            _ => {}
        }
    }
    state
}

#[cfg(test)]
mod tests {
    use super::*;
    use berimor_types::event::{EventSeq, ProcessInstanceId};
    use serde_json::json;

    fn patch(step_id: &str, changes: Value) -> Patch {
        Patch {
            step_id: step_id.to_string(),
            changes,
        }
    }

    fn step_applied_event(seq: u64, step_id: &str, payload: Value) -> Event {
        Event {
            seq: EventSeq(seq),
            process_instance: ProcessInstanceId("inst".into()),
            process_version: 1,
            kind: EventKind::StepApplied {
                step_id: step_id.to_string(),
            },
            payload,
            ts_ms: 0,
        }
    }

    #[test]
    fn apply_patch_sets_state_under_step_id() {
        let state = Value::Object(Map::new());
        let next = apply_patch(&state, &patch("classify", json!({"risk": 7})));
        assert_eq!(next, json!({"classify": {"risk": 7}}));
    }

    #[test]
    fn apply_patch_does_not_mutate_original_state() {
        let original = json!({"user": {"card_id": "c-1"}});
        let before = original.clone();
        let _ = apply_patch(&original, &patch("classify", json!({"risk": 1})));
        assert_eq!(
            original, before,
            "исходное значение состояния не должно меняться"
        );
    }

    #[test]
    fn apply_patch_replaces_previous_output_of_same_step() {
        let state = json!({"classify": {"risk": 3}});
        let next = apply_patch(&state, &patch("classify", json!({"risk": 9})));
        assert_eq!(
            next,
            json!({"classify": {"risk": 9}}),
            "повторный патч того же шага заменяет прошлый вывод целиком, не сливает поля"
        );
    }

    #[test]
    fn apply_patch_preserves_other_keys() {
        let state = json!({"user": {"card_id": "c-1"}});
        let next = apply_patch(&state, &patch("classify", json!({"risk": 2})));
        assert_eq!(
            next,
            json!({"user": {"card_id": "c-1"}, "classify": {"risk": 2}})
        );
    }

    #[test]
    fn fold_of_empty_log_is_empty_object() {
        assert_eq!(fold(&[]), json!({}));
    }

    #[test]
    fn apply_parallel_patch_writes_under_the_fork_and_branch_namespace() {
        let state = Value::Object(Map::new());
        let next = apply_parallel_patch(&state, "fanout", "classify", &json!({"risk": 7}));
        assert_eq!(
            next,
            json!({"parallel": {"fanout": {"classify": {"risk": 7}}}})
        );
    }

    #[test]
    fn apply_parallel_patch_of_a_second_branch_keeps_the_first() {
        let state = apply_parallel_patch(
            &Value::Object(Map::new()),
            "fanout",
            "classify",
            &json!({"risk": 7}),
        );
        let next = apply_parallel_patch(&state, "fanout", "answer", &json!({"reply": "ok"}));
        assert_eq!(
            next,
            json!({"parallel": {"fanout": {
                "classify": {"risk": 7},
                "answer": {"reply": "ok"}
            }}})
        );
    }

    #[test]
    fn apply_parallel_patch_does_not_disturb_top_level_state() {
        let state = json!({"user": {"card_id": "c-1"}});
        let next = apply_parallel_patch(&state, "fanout", "classify", &json!({"risk": 7}));
        assert_eq!(next["user"], json!({"card_id": "c-1"}));
    }

    #[test]
    fn apply_parallel_patch_does_not_mutate_the_original() {
        let original = Value::Object(Map::new());
        let before = original.clone();
        let _ = apply_parallel_patch(&original, "fanout", "classify", &json!({"risk": 7}));
        assert_eq!(original, before);
    }

    #[test]
    fn fold_handles_parallel_step_applied_events() {
        let events = vec![
            Event {
                seq: EventSeq(1),
                process_instance: ProcessInstanceId("inst".into()),
                process_version: 1,
                kind: EventKind::ParallelStepApplied {
                    fork_step_id: "fanout".into(),
                    branch_step_id: "classify".into(),
                },
                payload: json!({"risk": 7}),
                ts_ms: 0,
            },
            Event {
                seq: EventSeq(2),
                process_instance: ProcessInstanceId("inst".into()),
                process_version: 1,
                kind: EventKind::ParallelStepApplied {
                    fork_step_id: "fanout".into(),
                    branch_step_id: "answer".into(),
                },
                payload: json!({"reply": "ok"}),
                ts_ms: 0,
            },
        ];

        assert_eq!(
            fold(&events),
            json!({"parallel": {"fanout": {
                "classify": {"risk": 7},
                "answer": {"reply": "ok"}
            }}})
        );
    }

    #[test]
    fn fold_ignores_non_step_applied_events() {
        let events = vec![
            Event {
                seq: EventSeq(1),
                process_instance: ProcessInstanceId("inst".into()),
                process_version: 1,
                kind: EventKind::MediationRejected {
                    reason: "schema".into(),
                },
                payload: json!({"would_be": "ignored"}),
                ts_ms: 0,
            },
            step_applied_event(2, "classify", json!({"risk": 4})),
        ];
        assert_eq!(fold(&events), json!({"classify": {"risk": 4}}));
    }

    /// Математическое ядро I7: свёртка журнала равна тому же состоянию,
    /// которое получилось бы, применяя патчи последовательно по мере
    /// поступления. Полная версия этого свойства (с восстановлением после
    /// сбоя через storage) — задача P4, `docs/ROADMAP.md` §5.
    #[test]
    fn fold_matches_sequential_apply_patch() {
        let patches = [
            patch("classify", json!({"risk": 8, "category": "debt"})),
            patch("fetch_card_status", json!({"status": "active"})),
            patch("answer", json!({"reply": "..."})),
        ];

        let mut sequential_state = Value::Object(Map::new());
        for p in &patches {
            sequential_state = apply_patch(&sequential_state, p);
        }

        let events: Vec<Event> = patches
            .iter()
            .enumerate()
            .map(|(i, p)| step_applied_event((i + 1) as u64, &p.step_id, p.changes.clone()))
            .collect();

        assert_eq!(fold(&events), sequential_state);
    }

    #[test]
    fn fold_of_prefix_matches_apply_up_to_that_point() {
        let events = vec![
            step_applied_event(1, "classify", json!({"risk": 1})),
            step_applied_event(2, "fetch_card_status", json!({"status": "active"})),
        ];

        let after_first = fold(&events[..1]);
        let after_both = fold(&events);

        assert_eq!(after_first, json!({"classify": {"risk": 1}}));
        assert_eq!(
            after_both,
            json!({"classify": {"risk": 1}, "fetch_card_status": {"status": "active"}})
        );
    }
}
