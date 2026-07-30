//! `berimor-context-engine` — маршрутизатор → сборщик → оценщик бюджета.
//!
//! Источник: `docs/arch/ideal-agent-architecture.md` §3.5. ROADMAP: C1–C3.
//!
//! Минимальный вариант для Milestone 1 (`docs/ROADMAP.md` §18.3 п.4): три
//! механизма присутствуют как код-правила (не как заглушки-сигнатуры), но
//! набор слоёв ограничен тем, что реально существует в системе сейчас —
//! системные правила шага и состояние процесса. Слои памяти (личность,
//! проект, навыки, факты, сессия) появятся с Фазой 6; маршрутизатор уже
//! сейчас возвращает их в каноническом порядке, а сборщик отфильтровывает
//! отсутствующие — добавление слоя памяти не потребует менять порядок.

use berimor_types::model::ModelTier;

/// Слой контекста — единица сборки.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ContextLayer {
    pub name: String,
    pub content: String,
    pub weight: f32,
}

/// Канонические слои в ФИКСИРОВАННОМ порядке сборки (§3.5: «системные
/// правила → личность → проект → навыки → факты → сессия → текущая
/// задача»). Порядок — константа архитектуры, не параметр.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LayerKind {
    SystemRules,
    Personality,
    Project,
    Skills,
    Facts,
    Session,
    TaskState,
}

/// C1, маршрутизатор: класс задачи (тип шага) → набор слоёв. Правила —
/// код, не модель (§3.5, I1). Для структурированного шага с моделью —
/// системные правила и срез состояния; остальные слои отфильтрует
/// сборщик, пока соответствующие слои памяти не реализованы.
pub fn layers_for_step(step_kind: &str) -> Vec<LayerKind> {
    match step_kind {
        "llm_structured" => vec![LayerKind::SystemRules, LayerKind::TaskState],
        // У шагов без модели контекста для модели нет вовсе — не ошибка,
        // а следствие «если шаг не требует понимания текста — модели в
        // нём нет» (executors.md §2).
        _ => vec![],
    }
}

/// C2, сборщик: канонический порядок слоёв независимо от порядка входа.
/// Слои, отсутствующие во входе (не реализованные ещё слои памяти),
/// молча пропускаются — порядок оставшихся неизменен.
pub fn assemble(mut layers: Vec<(LayerKind, ContextLayer)>) -> Vec<ContextLayer> {
    layers.sort_by_key(|(kind, _)| *kind);
    layers.into_iter().map(|(_, layer)| layer).collect()
}

/// C3, оценщик бюджета: класс модели определяет бюджет контекста (§3.5:
/// «слабая модель получает более короткий и более структурированный
/// контекст»). Значения — стартовые константы кода (в символах), калибровка
/// — офлайн-оценкой (Фаза 9), не самой моделью (ADR-0010).
pub fn budget_chars(tier: ModelTier) -> usize {
    match tier {
        ModelTier::Weak => 2_000,
        ModelTier::Medium => 8_000,
        ModelTier::Strong => 32_000,
    }
}

/// Суммарный размер слоёв — то, что сравнивается с [`budget_chars`].
pub fn total_chars(layers: &[ContextLayer]) -> usize {
    layers.iter().map(|l| l.content.len()).sum()
}

/// `build(step, state) → context` — единственный путь чтения памяти в
/// структурированных шагах (`memory-model.md` §3): у модели нет инструмента
/// «сама поищи в памяти».
pub trait ContextBuilder {
    fn build(
        &self,
        step_kind: &str,
        tier: ModelTier,
        state: &serde_json::Value,
    ) -> Vec<ContextLayer>;
}

/// Минимальный построитель для Milestone 1: маршрутизатор + сборщик
/// реальные, содержимое слоёв — то, что есть: системное правило шага и
/// состояние целиком одним слоем (`docs/ROADMAP.md` §18.3 п.4 допускает
/// именно это; полноценный срез по весам — за пределами milestone).
pub struct SimpleContextBuilder;

impl ContextBuilder for SimpleContextBuilder {
    fn build(
        &self,
        step_kind: &str,
        _tier: ModelTier,
        state: &serde_json::Value,
    ) -> Vec<ContextLayer> {
        let mut available: Vec<(LayerKind, ContextLayer)> = Vec::new();
        for kind in layers_for_step(step_kind) {
            let layer = match kind {
                LayerKind::SystemRules => ContextLayer {
                    name: "system_rules".into(),
                    content: "Отвечай строго JSON по схеме контракта. Без пояснений, без markdown."
                        .into(),
                    weight: 1.0,
                },
                LayerKind::TaskState => ContextLayer {
                    name: "task_state".into(),
                    content: serde_json::to_string_pretty(state)
                        .expect("состояние процесса всегда сериализуемо"),
                    weight: 1.0,
                },
                // Слои памяти — Фаза 6; маршрутизатор их уже перечисляет,
                // здесь им просто нечего наполнять.
                _ => continue,
            };
            available.push((kind, layer));
        }
        assemble(available)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn layer(name: &str) -> ContextLayer {
        ContextLayer {
            name: name.into(),
            content: String::new(),
            weight: 1.0,
        }
    }

    #[test]
    fn router_maps_structured_step_to_documented_layers() {
        let layers = layers_for_step("llm_structured");
        assert_eq!(layers, vec![LayerKind::SystemRules, LayerKind::TaskState]);
    }

    #[test]
    fn router_gives_no_model_context_to_model_less_steps() {
        assert!(layers_for_step("tool").is_empty());
        assert!(layers_for_step("branch").is_empty());
    }

    #[test]
    fn assembler_enforces_canonical_order_regardless_of_input_order() {
        let shuffled = vec![
            (LayerKind::TaskState, layer("task")),
            (LayerKind::Session, layer("session")),
            (LayerKind::SystemRules, layer("rules")),
            (LayerKind::Skills, layer("skills")),
        ];
        let ordered = assemble(shuffled);
        let names: Vec<&str> = ordered.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(names, vec!["rules", "skills", "session", "task"]);
    }

    #[test]
    fn budget_grows_with_model_tier() {
        assert!(budget_chars(ModelTier::Weak) < budget_chars(ModelTier::Medium));
        assert!(budget_chars(ModelTier::Medium) < budget_chars(ModelTier::Strong));
    }

    #[test]
    fn builder_returns_system_rules_and_full_state_as_layers() {
        let state = json!({"user": {"card_id": "c-1"}});
        let layers = SimpleContextBuilder.build("llm_structured", ModelTier::Weak, &state);

        assert_eq!(layers.len(), 2);
        assert_eq!(layers[0].name, "system_rules");
        assert_eq!(layers[1].name, "task_state");
        assert!(layers[1].content.contains("c-1"));
    }

    #[test]
    fn builder_gives_nothing_to_tool_steps() {
        let layers = SimpleContextBuilder.build("tool", ModelTier::Weak, &json!({}));
        assert!(layers.is_empty());
    }

    #[test]
    fn total_chars_sums_layer_contents() {
        let layers = vec![
            ContextLayer {
                name: "a".into(),
                content: "123".into(),
                weight: 1.0,
            },
            ContextLayer {
                name: "b".into(),
                content: "45".into(),
                weight: 1.0,
            },
        ];
        assert_eq!(total_chars(&layers), 5);
    }
}
