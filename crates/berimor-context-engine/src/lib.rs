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

pub mod memory_builder;

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
    /// Граф сущностей (memory-model.md §4): прецеденты и связи («все
    /// инциденты этого поставщика») — поверх семантического слоя, рядом
    /// с сессионным контекстом, но до состояния текущей задачи.
    EntityGraph,
    TaskState,
}

/// C1, маршрутизатор: класс задачи (тип шага) → набор слоёв. Правила —
/// код, не модель (§3.5, I1). Для структурированного шага с моделью —
/// системные правила, срез состояния и слои Skills/Session (Фаза 6:
/// процедурная и эпизодическая память). Personality/Project/Facts
/// остаются вне списка — в системе нет источника профиля/арендатора и
/// нет провайдера эмбеддингов (честный пробел, не забытая строка);
/// добавление им места не требует менять порядок, только список.
pub fn layers_for_step(step_kind: &str) -> Vec<LayerKind> {
    match step_kind {
        // Техдолг TD4.2 (`docs/audit-2026-07-31.md`): раньше матчилось
        // только "llm_structured" — "agent_step"/"codeact" (E9/E8, оба
        // ДЕЙСТВИТЕЛЬНО несут модель, вызывают `context.build(...)` тем
        // же путём) попадали в `_ => vec![]` и получали пустой контекст:
        // ни системных правил, ни состояния задачи, ни слоёв памяти —
        // не осознанный пробел, а следствие того, что роутер не был
        // расширен при добавлении этих двух исполнителей. Набор слоёв —
        // тот же, что у `llm_structured`: ничего специфичного, что
        // отличало бы потребность этих шагов в контексте, ни в одном
        // документе не описано.
        "llm_structured" | "agent_step" | "codeact" => vec![
            LayerKind::SystemRules,
            LayerKind::Skills,
            LayerKind::Session,
            LayerKind::EntityGraph,
            LayerKind::TaskState,
        ],
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

/// Пометка усечения слоя по бюджету — видимая модели, не тихий обрыв.
const TRUNC_MARK: &str = "\n…[слой усечён по бюджету контекста]";

/// C3 в действии (аудит 4.3): применяет бюджет класса модели к собранным
/// слоям — до этого бюджет существовал только как API без потребителей.
///
/// Порядок жертв — по каноническому порядку слоёв от необязательных к
/// обязательным: Skills и Session сбрасываются целиком первыми (память —
/// усиление, не необходимость), `task_state` не сбрасывается, а усечётся
/// с пометкой (шагу без состояния хуже, чем с частью), `system_rules`
/// не сбрасывается и не урезается никогда — потерять форму ответа хуже,
/// чем потерять контекст. Детерминированно: одинаковый вход → одинаковый
/// набор слоёв, никаких эвристик «релевантности» (I1).
pub fn apply_budget(mut layers: Vec<ContextLayer>, tier: ModelTier) -> Vec<ContextLayer> {
    let budget = budget_chars(tier);
    // Сброс необязательных слоёв целиком, пока не уложимся в бюджет.
    while total_chars(&layers) > budget {
        let Some(pos) = layers
            .iter()
            .position(|l| l.name != "system_rules" && l.name != "task_state")
        else {
            break;
        };
        layers.remove(pos);
    }
    // Усечение остатка сверх бюджета (прежде всего task_state) — по
    // границе символа (UTF-8), с видимой пометкой.
    let mut used = 0usize;
    for layer in layers.iter_mut() {
        if layer.name == "system_rules" {
            used += layer.content.len();
            continue;
        }
        let remaining = budget.saturating_sub(used);
        if layer.content.len() > remaining {
            // LOW независимого ревью §20.5: при remaining < пометки резать
            // БЕЗ пометки — иначе слой превышал бы отведённое ею самой.
            if remaining < TRUNC_MARK.len() {
                let mut cut = remaining;
                while cut > 0 && !layer.content.is_char_boundary(cut) {
                    cut -= 1;
                }
                layer.content.truncate(cut);
            } else {
                let mut cut = remaining - TRUNC_MARK.len();
                while cut > 0 && !layer.content.is_char_boundary(cut) {
                    cut -= 1;
                }
                layer.content.truncate(cut);
                layer.content.push_str(TRUNC_MARK);
            }
        }
        used += layer.content.len();
    }
    layers
}

/// `build(step, state, task_hint) → context` — единственный путь чтения
/// памяти в структурированных шагах (`memory-model.md` §3): у модели нет
/// инструмента «сама поищи в памяти». `task_hint` — короткий текстовый
/// сигнал задачи, по которому слой Session ищет релевантные прошлые
/// сессии, не разбирая `state` эвристиками внутри построителя. ОБЯЗАН
/// быть значением, которое вызывающий код реально журналирует (иначе
/// поиск по нему декоративен — найдено независимым ревью интеграции
/// CLI-M1/M2/M3): `step_id` подходит (журналируется в `StepApplied`),
/// имя контракта — нет.
pub trait ContextBuilder {
    fn build(
        &self,
        step_kind: &str,
        tier: ModelTier,
        state: &serde_json::Value,
        task_hint: &str,
    ) -> Vec<ContextLayer>;
}

/// Общая часть слоёв `SystemRules`/`TaskState`, одинаковая для
/// [`SimpleContextBuilder`] и любого построителя, добавляющего слои
/// памяти поверх — вынесена, чтобы не дублировать текст системного
/// правила в двух местах.
pub(crate) fn base_layer(kind: LayerKind, state: &serde_json::Value) -> Option<ContextLayer> {
    match kind {
        LayerKind::SystemRules => Some(ContextLayer {
            name: "system_rules".into(),
            content: "Отвечай строго JSON по схеме контракта. Без пояснений, без markdown.".into(),
            weight: 1.0,
        }),
        LayerKind::TaskState => Some(ContextLayer {
            name: "task_state".into(),
            content: serde_json::to_string_pretty(state)
                .expect("состояние процесса всегда сериализуемо"),
            weight: 1.0,
        }),
        _ => None,
    }
}

/// Минимальный построитель: маршрутизатор + сборщик реальные, но слоям
/// памяти (Skills/Session) нечего наполнять без источника — используется
/// там, где память не подключена (`docs/ROADMAP.md` §18.3 п.4).
pub struct SimpleContextBuilder;

impl ContextBuilder for SimpleContextBuilder {
    fn build(
        &self,
        step_kind: &str,
        tier: ModelTier,
        state: &serde_json::Value,
        _task_hint: &str,
    ) -> Vec<ContextLayer> {
        let available: Vec<(LayerKind, ContextLayer)> = layers_for_step(step_kind)
            .into_iter()
            .filter_map(|kind| base_layer(kind, state).map(|layer| (kind, layer)))
            .collect();
        // Бюджет класса модели — на единственном пути сборки (аудит 4.3),
        // не опция вызывающего кода.
        apply_budget(assemble(available), tier)
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
        assert_eq!(
            layers,
            vec![
                LayerKind::SystemRules,
                LayerKind::Skills,
                LayerKind::Session,
                LayerKind::EntityGraph,
                LayerKind::TaskState,
            ]
        );
    }

    #[test]
    fn router_gives_no_model_context_to_model_less_steps() {
        assert!(layers_for_step("tool").is_empty());
        assert!(layers_for_step("branch").is_empty());
    }

    /// Техдолг TD4.2: `agent_step`/`codeact` (E9/E8) ДЕЙСТВИТЕЛЬНО несут
    /// модель и вызывают `context.build(...)` тем же путём, что
    /// `llm_structured`, — раньше попадали в `_ => vec![]` и получали
    /// пустой контекст.
    #[test]
    fn router_gives_the_same_layers_to_agent_step_and_codeact_as_to_llm_structured() {
        let expected = layers_for_step("llm_structured");
        assert_eq!(layers_for_step("agent_step"), expected);
        assert_eq!(layers_for_step("codeact"), expected);
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

    /// Аудит 4.3: перерасход сбрасывает Skills/Session целиком первыми,
    /// SystemRules остаётся всегда, TaskState усечётся с пометкой.
    #[test]
    fn apply_budget_drops_memory_layers_then_truncates_state() {
        let huge = "x".repeat(10_000);
        let layers = vec![
            ContextLayer {
                name: "system_rules".into(),
                content: "правила".into(),
                weight: 1.0,
            },
            ContextLayer {
                name: "skills".into(),
                content: huge.clone(),
                weight: 1.0,
            },
            ContextLayer {
                name: "session".into(),
                content: huge.clone(),
                weight: 1.0,
            },
            ContextLayer {
                name: "task_state".into(),
                content: huge,
                weight: 1.0,
            },
        ];

        let result = apply_budget(layers, ModelTier::Weak);

        let names: Vec<&str> = result.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(names, vec!["system_rules", "task_state"]);
        assert!(total_chars(&result) <= budget_chars(ModelTier::Weak));
        let state = &result[1];
        assert!(
            state.content.contains("усечён по бюджету"),
            "{}",
            state.content
        );
    }

    /// Усечение — по границе символа UTF-8 (кириллица на границе реза не
    /// должна давать битую строку).
    #[test]
    fn truncation_respects_char_boundaries() {
        let layers = vec![
            ContextLayer {
                name: "system_rules".into(),
                content: "r".into(),
                weight: 1.0,
            },
            ContextLayer {
                name: "task_state".into(),
                content: "ё".repeat(5_000), // 2 байта на символ
                weight: 1.0,
            },
        ];
        let result = apply_budget(layers, ModelTier::Weak);
        // Само существование String после truncate — доказательство:
        // truncate вне границы символа паникует.
        assert!(result[1].content.ends_with(']'));
    }

    /// Сильному классу тот же вход достаётся целиком — бюджет не режет
    /// то, что влезает (§3.5: «слабая — более короткий», не «всем резать»).
    #[test]
    fn strong_tier_keeps_what_fits() {
        let state = json!({"user": {"card_id": "c-1"}});
        let layers = SimpleContextBuilder.build("llm_structured", ModelTier::Strong, &state, "");
        let names: Vec<&str> = layers.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(names, vec!["system_rules", "task_state"]);
        assert!(layers[1].content.contains("c-1"));
    }

    #[test]
    fn builder_returns_system_rules_and_full_state_as_layers() {
        let state = json!({"user": {"card_id": "c-1"}});
        let layers = SimpleContextBuilder.build("llm_structured", ModelTier::Weak, &state, "");

        // Skills/Session перечислены маршрутизатором, но SimpleContextBuilder
        // не умеет их наполнять — assemble() их молча опускает.
        assert_eq!(layers.len(), 2);
        assert_eq!(layers[0].name, "system_rules");
        assert_eq!(layers[1].name, "task_state");
        assert!(layers[1].content.contains("c-1"));
    }

    #[test]
    fn builder_gives_nothing_to_tool_steps() {
        let layers = SimpleContextBuilder.build("tool", ModelTier::Weak, &json!({}), "");
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
