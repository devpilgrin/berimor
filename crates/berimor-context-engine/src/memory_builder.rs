//! `MemoryContextBuilder` — слои Skills/Session поверх базовых слоёв.
//!
//! Источник: `docs/arch/memory-model.md` §3. Интеграция Фазы 6 в
//! `berimor run` (ROADMAP: пункт «интеграция», не отдельная буква фазы —
//! см. `.remember/remember.md`).
//!
//! Facts (семантическая память) и Personality/Project сюда сознательно не
//! входят: `Facts` требует эмбеддинг-запрос (`SemanticStore::hybrid_search`),
//! а провайдера эмбеддингов в системе нет; Personality/Project требуют
//! понятия профиля/арендатора, которого нет в конфигурации CLI. Оба —
//! задокументированный пробел, не забытая строка (тот же класс, что
//! `token_budget`/`cost_budget` в P6).

use crate::{assemble, base_layer, layers_for_step, ContextBuilder, ContextLayer, LayerKind};
use berimor_memory::{episodic, procedural::SkillSummary};
use berimor_storage::EpisodicSearch;
use berimor_types::model::ModelTier;
use serde_json::Value;

/// Построитель поверх уже открытого журнала (тот же `SqliteEventLog`, что
/// и процесс-журнал — `EpisodicSearch` реализован на нём напрямую,
/// отдельного подключения к БД не требуется) и уже разобранного списка
/// навыков (чтение файлов — дело вызывающего кода, не построителя).
pub struct MemoryContextBuilder<'a> {
    pub episodic: &'a dyn EpisodicSearch,
    pub skills: &'a [SkillSummary],
    /// Верхняя граница числа сессий в слое `Session` (`episodic::search_sessions`).
    pub session_search_limit: usize,
}

impl ContextBuilder for MemoryContextBuilder<'_> {
    fn build(
        &self,
        step_kind: &str,
        _tier: ModelTier,
        state: &Value,
        task_hint: &str,
    ) -> Vec<ContextLayer> {
        let available: Vec<(LayerKind, ContextLayer)> = layers_for_step(step_kind)
            .into_iter()
            .filter_map(|kind| {
                let layer = match kind {
                    LayerKind::Skills => self.skills_layer(),
                    LayerKind::Session => self.session_layer(task_hint),
                    other => base_layer(other, state),
                };
                layer.map(|layer| (kind, layer))
            })
            .collect();
        assemble(available)
    }
}

impl MemoryContextBuilder<'_> {
    /// «Описание всегда в доступе» (`memory-model.md` §3) — весь список
    /// навыков без фильтрации, только их описания (`SkillSummary` не
    /// содержит тела — гарантия типов, не соглашение).
    fn skills_layer(&self) -> Option<ContextLayer> {
        if self.skills.is_empty() {
            return None;
        }
        let content = self
            .skills
            .iter()
            .map(|s| format!("- {} (v{}): {}", s.name, s.version, s.description))
            .collect::<Vec<_>>()
            .join("\n");
        Some(ContextLayer {
            name: "skills".into(),
            content,
            weight: 1.0,
        })
    }

    /// Ошибка поиска (например, повреждённый индекс) не должна ронять шаг
    /// — пустой слой, не `Err`; тот же принцип, что у `interpolate()` в
    /// `berimor-cli/src/run.rs` («неразрешимый путь остаётся как есть»).
    fn session_layer(&self, task_hint: &str) -> Option<ContextLayer> {
        if task_hint.is_empty() {
            return None;
        }
        let sessions =
            episodic::search_sessions(self.episodic, task_hint, self.session_search_limit)
                .unwrap_or_default();
        if sessions.is_empty() {
            return None;
        }
        let content = sessions
            .iter()
            .map(|s| {
                let hits = s
                    .hits
                    .iter()
                    .map(|h| format!("{:?}: {}", h.kind, h.payload))
                    .collect::<Vec<_>>()
                    .join("; ");
                format!("сессия {}: {}", s.session.0, hits)
            })
            .collect::<Vec<_>>()
            .join("\n");
        Some(ContextLayer {
            name: "session".into(),
            content,
            weight: 1.0,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use berimor_storage::{EventLog, SqliteEventLog};
    use berimor_types::event::{Event, EventKind, ProcessInstanceId};
    use serde_json::json;

    fn skill(name: &str, description: &str) -> SkillSummary {
        SkillSummary {
            name: name.into(),
            version: 1,
            description: description.into(),
        }
    }

    #[test]
    fn skills_layer_lists_every_skill_by_description_not_body() {
        let storage = SqliteEventLog::open_in_memory().unwrap();
        let skills = vec![
            skill("card-status-lookup", "Проверка статуса доставки карты"),
            skill("refund", "Оформление возврата"),
        ];
        let builder = MemoryContextBuilder {
            episodic: &storage,
            skills: &skills,
            session_search_limit: 5,
        };

        let layers = builder.build("llm_structured", ModelTier::Weak, &json!({}), "");
        let skills_layer = layers.iter().find(|l| l.name == "skills").unwrap();
        assert!(skills_layer.content.contains("card-status-lookup"));
        assert!(skills_layer
            .content
            .contains("Проверка статуса доставки карты"));
        assert!(skills_layer.content.contains("refund"));
    }

    #[test]
    fn empty_skill_list_produces_no_skills_layer() {
        let storage = SqliteEventLog::open_in_memory().unwrap();
        let builder = MemoryContextBuilder {
            episodic: &storage,
            skills: &[],
            session_search_limit: 5,
        };

        let layers = builder.build("llm_structured", ModelTier::Weak, &json!({}), "");
        assert!(!layers.iter().any(|l| l.name == "skills"));
    }

    #[test]
    fn session_layer_finds_matching_past_events_by_task_hint() {
        let storage = SqliteEventLog::open_in_memory().unwrap();
        let instance = ProcessInstanceId("run-1".into());
        storage
            .append(Event::new(
                instance.clone(),
                1,
                EventKind::StepApplied {
                    step_id: "classify".into(),
                },
                json!({"card_id": "c-1", "note": "SupportReply"}),
            ))
            .unwrap();

        let skills = Vec::new();
        let builder = MemoryContextBuilder {
            episodic: &storage,
            skills: &skills,
            session_search_limit: 5,
        };

        let layers = builder.build(
            "llm_structured",
            ModelTier::Weak,
            &json!({}),
            "SupportReply",
        );
        let session_layer = layers.iter().find(|l| l.name == "session");
        assert!(session_layer.is_some(), "ожидалось совпадение по сессии");
        assert!(session_layer.unwrap().content.contains("run-1"));
    }

    #[test]
    fn empty_task_hint_produces_no_session_layer() {
        let storage = SqliteEventLog::open_in_memory().unwrap();
        let skills = Vec::new();
        let builder = MemoryContextBuilder {
            episodic: &storage,
            skills: &skills,
            session_search_limit: 5,
        };

        let layers = builder.build("llm_structured", ModelTier::Weak, &json!({}), "");
        assert!(!layers.iter().any(|l| l.name == "session"));
    }

    #[test]
    fn no_match_produces_no_session_layer() {
        let storage = SqliteEventLog::open_in_memory().unwrap();
        let skills = Vec::new();
        let builder = MemoryContextBuilder {
            episodic: &storage,
            skills: &skills,
            session_search_limit: 5,
        };

        let layers = builder.build(
            "llm_structured",
            ModelTier::Weak,
            &json!({}),
            "nonexistent_term_xyz",
        );
        assert!(!layers.iter().any(|l| l.name == "session"));
    }
}
