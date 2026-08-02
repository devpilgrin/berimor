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

/// Вхождение `needle` как отдельного токена: символы по краям матча не
/// из [A-Za-z0-9_]. Без regex-зависимости — два ручных сравнения.
fn contains_token(haystack: &str, needle: &str) -> bool {
    let boundary = |b: Option<u8>| !b.is_some_and(|b| b.is_ascii_alphanumeric() || b == b'_');
    haystack.match_indices(needle).any(|(pos, _)| {
        let before = pos.checked_sub(1).map(|i| haystack.as_bytes()[i]);
        let after = haystack.as_bytes().get(pos + needle.len()).copied();
        boundary(before) && boundary(after)
    })
}
use berimor_memory::{episodic, procedural::SkillSummary};
use berimor_storage::{EntityGraphStore, EpisodicSearch};
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
    /// Граф сущностей (ROADMAP §20.5, memory-model.md §4): `Some` — слой
    /// `EntityGraph` наполняется из хранилища, `None` — слой отключён.
    /// «Включается профилем процесса, не глобально» — решение принимает
    /// вызывающий код (конфигурация), не построитель.
    pub entity_graph: Option<&'a dyn EntityGraphStore>,
    /// Реестр секретов запуска (S5): контент слоя графа маскируется
    /// перед попаданием в контекст — записано оно может быть ВНЕ
    /// контрактов Mediation (контрактных продюсеров графа пока нет,
    /// §20.5), т.е. читается как недоверенное. Тот же принцип, что у
    /// маскировки вывода инструментов: граница «данные → модель».
    pub masker: Option<&'a berimor_secrets::Masker>,
}

impl ContextBuilder for MemoryContextBuilder<'_> {
    fn build(
        &self,
        step_kind: &str,
        tier: ModelTier,
        state: &Value,
        task_hint: &str,
    ) -> Vec<ContextLayer> {
        let available: Vec<(LayerKind, ContextLayer)> = layers_for_step(step_kind)
            .into_iter()
            .filter_map(|kind| {
                let layer = match kind {
                    LayerKind::Skills => self.skills_layer(),
                    LayerKind::Session => self.session_layer(task_hint),
                    LayerKind::EntityGraph => self.entity_layer(task_hint, state),
                    other => base_layer(other, state),
                };
                layer.map(|layer| (kind, layer))
            })
            .collect();
        // Бюджет класса модели — на единственном пути сборки (аудит 4.3):
        // при перерасходе Skills/Session уходят первыми.
        crate::apply_budget(assemble(available), tier)
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

    /// Слой графа сущностей (ROADMAP §20.5, memory-model.md §4):
    /// релевантные задаче узлы + их соседи в одно ребро — та выборка
    /// «все инциденты этого поставщика», которую векторный поиск не
    /// умеет, в детерминированной форме.
    ///
    /// Релевантность — код, не эмбеддинги (тот же честный пробел, что у
    /// слоя Facts): узел релевантен, если его id встречается подстрокой в
    /// `task_hint` или сериализованном состоянии; id короче 3 символов
    /// не матчится (иначе короткий id шумит на любой JSON). Ошибка
    /// хранилища — пустой слой, не падение шага (принцип session_layer).
    /// Требования к размеру графа (MEDIUM ревью §20.5): чтение —
    /// полный скан `all_nodes`/`all_edges` на КАЖДЫЙ llm-шаг; это
    /// осознанно приемлемо для графа размера «сотни–тысячи узлов»
    /// (домены прецедентов, §4), не для графа-озера — точечный lookup
    /// по id в `EntityGraphStore` будет нужен раньше таких масштабов.
    /// Известный предел релевантности (LOW ревью): id, содержащий
    /// JSON-экранируемые символы (`"`, `\`, управляющие), в
    /// сериализованном haystack не матчится — молчаливый пропуск
    /// (кириллица не затронута).
    fn entity_layer(&self, task_hint: &str, state: &Value) -> Option<ContextLayer> {
        const MAX_GRAPH_NODES: usize = 32;
        let store = self.entity_graph?;
        let haystack = format!(
            "{task_hint}\n{}",
            serde_json::to_string(state).unwrap_or_default()
        );
        let nodes = store.all_nodes().unwrap_or_default();
        let edges = store.all_edges().unwrap_or_default();
        if nodes.is_empty() {
            return None;
        }

        // MEDIUM независимого ревью §20.5: подстрочный матч ловил чужие
        // сущности (`card_102` внутри `card_1029`, `101` внутри
        // `10150`) — матч по ГРАНИЦЕ ТОКЕНА (соседние символы не из
        // [A-Za-z0-9_]), не по подстроке.
        let mut included: Vec<&berimor_storage::NodeRecord> = nodes
            .iter()
            .filter(|n| n.id.len() >= 3 && contains_token(&haystack, &n.id))
            .collect();
        // Индексы — один раз (MEDIUM ревью: линейный поиск в цикле по
        // рёбрам давал O(E×N) на каждый llm-шаг).
        let by_id: std::collections::HashMap<&str, &berimor_storage::NodeRecord> =
            nodes.iter().map(|n| (n.id.as_str(), n)).collect();
        let mut included_ids: std::collections::HashSet<&str> =
            included.iter().map(|n| n.id.as_str()).collect();
        // Соседи в одно ребро — прецеденты/связи релевантного узла.
        for edge in &edges {
            let other = if included_ids.contains(edge.source.as_str()) {
                Some(edge.target.as_str())
            } else if included_ids.contains(edge.target.as_str()) {
                Some(edge.source.as_str())
            } else {
                None
            };
            if let Some(other_id) = other {
                if !included_ids.contains(other_id) {
                    if let Some(node) = by_id.get(other_id) {
                        included.push(node);
                        included_ids.insert(other_id);
                    }
                }
            }
        }
        if included.is_empty() {
            return None;
        }
        included.truncate(MAX_GRAPH_NODES);

        let ids: Vec<&str> = included.iter().map(|n| n.id.as_str()).collect();
        let mut lines: Vec<String> = included
            .iter()
            .map(|n| format!("узел {} ({}): {}", n.id, n.node_type, n.properties))
            .collect();
        for edge in &edges {
            if ids.contains(&edge.source.as_str()) && ids.contains(&edge.target.as_str()) {
                lines.push(format!(
                    "ребро {}: {} -> {}",
                    edge.edge_type, edge.source, edge.target
                ));
            }
        }
        // HIGH независимого ревью §20.5: properties узлов — недоверенный
        // текст (записан вне Mediation), секрет в них достигал модели.
        let content = lines.join("\n");
        let content = match self.masker {
            Some(masker) => masker.mask_text(&content),
            None => content,
        };
        Some(ContextLayer {
            name: "entity_graph".into(),
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
            entity_graph: None,
            masker: None,
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
            entity_graph: None,
            masker: None,
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
            entity_graph: None,
            masker: None,
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
            entity_graph: None,
            masker: None,
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
            entity_graph: None,
            masker: None,
        };

        let layers = builder.build(
            "llm_structured",
            ModelTier::Weak,
            &json!({}),
            "nonexistent_term_xyz",
        );
        assert!(!layers.iter().any(|l| l.name == "session"));
    }

    use berimor_storage::{EdgeRecord, NodeRecord};

    fn node(id: &str, node_type: &str, props: serde_json::Value) -> NodeRecord {
        NodeRecord {
            id: id.into(),
            node_type: node_type.into(),
            properties: props,
        }
    }

    fn edge(id: &str, edge_type: &str, source: &str, target: &str) -> EdgeRecord {
        EdgeRecord {
            id: id.into(),
            edge_type: edge_type.into(),
            source: source.into(),
            target: target.into(),
            properties: json!({}),
        }
    }

    /// §20.5: узел, чей id встречается в состоянии, попадает в слой
    /// вместе с соседом по ребру («все инциденты этого поставщика»);
    /// несвязанный узел — нет.
    #[test]
    fn entity_layer_includes_relevant_node_and_its_neighbor() {
        let storage = SqliteEventLog::open_in_memory().unwrap();
        storage
            .upsert_node(&node("card_1029", "card", json!({"holder": "Иван"})))
            .unwrap();
        storage
            .upsert_node(&node("batch_77", "batch", json!({"status": "issued"})))
            .unwrap();
        storage
            .upsert_node(&node("unrelated_999", "batch", json!({})))
            .unwrap();
        storage
            .upsert_edge(&edge("e1", "issued_in", "card_1029", "batch_77"))
            .unwrap();

        let builder = MemoryContextBuilder {
            episodic: &storage,
            skills: &[],
            session_search_limit: 5,
            entity_graph: Some(&storage),
            masker: None,
        };

        let layers = builder.build(
            "llm_structured",
            ModelTier::Strong,
            &json!({"user": {"card_id": "card_1029"}}),
            "classify",
        );
        let layer = layers
            .iter()
            .find(|l| l.name == "entity_graph")
            .expect("слой графа обязан быть");
        assert!(layer.content.contains("card_1029"), "{}", layer.content);
        assert!(layer.content.contains("batch_77"), "{}", layer.content);
        assert!(
            layer
                .content
                .contains("ребро issued_in: card_1029 -> batch_77"),
            "{}",
            layer.content
        );
        assert!(
            !layer.content.contains("unrelated_999"),
            "{}",
            layer.content
        );
    }

    /// Граф отключён (None) или пуст — слоя нет вовсе, не пустой слой.
    #[test]
    fn entity_layer_absent_when_disabled_or_empty() {
        let storage = SqliteEventLog::open_in_memory().unwrap();
        let off = MemoryContextBuilder {
            episodic: &storage,
            skills: &[],
            session_search_limit: 5,
            entity_graph: None,
            masker: None,
        };
        let layers = off.build("llm_structured", ModelTier::Strong, &json!({}), "classify");
        assert!(!layers.iter().any(|l| l.name == "entity_graph"));

        let on_empty = MemoryContextBuilder {
            episodic: &storage,
            skills: &[],
            session_search_limit: 5,
            entity_graph: Some(&storage),
            masker: None,
        };
        let layers = on_empty.build("llm_structured", ModelTier::Strong, &json!({}), "classify");
        assert!(!layers.iter().any(|l| l.name == "entity_graph"));
    }

    /// MEDIUM ревью §20.5: `card_102` не должен матчиться на состояние
    /// с `card_1029` (и наоборот), числовой id `101` — на `10150`.
    #[test]
    fn entity_relevance_matches_token_boundaries_not_substrings() {
        let storage = SqliteEventLog::open_in_memory().unwrap();
        storage
            .upsert_node(&node("card_102", "card", json!({"note": "ЧУЖАЯ КАРТА"})))
            .unwrap();
        storage
            .upsert_node(&node("101", "metric", json!({"note": "ЧИСЛО"})))
            .unwrap();
        let builder = MemoryContextBuilder {
            episodic: &storage,
            skills: &[],
            session_search_limit: 5,
            entity_graph: Some(&storage),
            masker: None,
        };

        let layers = builder.build(
            "llm_structured",
            ModelTier::Strong,
            &json!({"card_id": "card_1029", "amount": 10150}),
            "classify",
        );
        assert!(
            !layers.iter().any(|l| l.name == "entity_graph"),
            "чужие id не должны матчиться: {:?}",
            layers.iter().map(|l| &l.name).collect::<Vec<_>>()
        );
    }

    /// HIGH ревью §20.5: секрет в properties узла маскируется тем же
    /// реестром, что вывод инструментов, — до модели доходит алиас.
    #[test]
    fn entity_layer_content_is_masked() {
        let storage = SqliteEventLog::open_in_memory().unwrap();
        storage
            .upsert_node(&node(
                "card_1029",
                "card",
                json!({"note": "токен sk-live-FAKESECRET12345"}),
            ))
            .unwrap();
        let mut masker = berimor_secrets::Masker::new();
        masker.register(berimor_secrets::Secret::new(
            "sk-live-FAKESECRET12345".to_string(),
        ));
        let builder = MemoryContextBuilder {
            episodic: &storage,
            skills: &[],
            session_search_limit: 5,
            entity_graph: Some(&storage),
            masker: Some(&masker),
        };

        let layers = builder.build(
            "llm_structured",
            ModelTier::Strong,
            &json!({"card_id": "card_1029"}),
            "classify",
        );
        let layer = layers
            .iter()
            .find(|l| l.name == "entity_graph")
            .expect("слой обязан быть");
        assert!(
            !layer.content.contains("sk-live-FAKESECRET12345"),
            "{}",
            layer.content
        );
        assert!(layer.content.contains("‹secret›"), "{}", layer.content);
    }

    /// Канонический порядок (memory-model.md §3): граф — после Session,
    /// до TaskState.
    #[test]
    fn entity_graph_sits_between_session_and_task_state() {
        let storage = SqliteEventLog::open_in_memory().unwrap();
        storage
            .upsert_node(&node("card_1029", "card", json!({})))
            .unwrap();
        let builder = MemoryContextBuilder {
            episodic: &storage,
            skills: &[],
            session_search_limit: 5,
            entity_graph: Some(&storage),
            masker: None,
        };
        let layers = builder.build(
            "llm_structured",
            ModelTier::Strong,
            &json!({"card_id": "card_1029"}),
            "classify",
        );
        let names: Vec<&str> = layers.iter().map(|l| l.name.as_str()).collect();
        let graph_pos = names.iter().position(|n| *n == "entity_graph").unwrap();
        let state_pos = names.iter().position(|n| *n == "task_state").unwrap();
        assert!(graph_pos < state_pos, "{names:?}");
    }
}
