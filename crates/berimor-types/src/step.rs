//! Граф процесса и типы шагов.
//!
//! Источник: `docs/arch/process-engine.md` §2. Набор типов шагов намеренно
//! ограничен и закрыт — «выразительность приносится в жертву предсказуемости».
//! ROADMAP: P1, P2.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Ограниченный набор типов шагов. Ветвление (`Branch`) всегда работает на
/// уже валидированных полях состояния — модель никогда не выбирает ветку (I1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StepKind {
    Sequential,
    Parallel {
        branches: Vec<String>,
    },
    Loop {
        condition: String,
    },
    Branch {
        on: String,
        cases: BTreeMap<String, String>,
    },
    Tool {
        tool: String,
        /// Шаблон аргументов, не разрешённые значения — резолвится
        /// движком из состояния при исполнении (executors.md §2, ROADMAP E1).
        #[serde(default)]
        args: serde_json::Value,
    },
    LlmStructured {
        contract: String,
        #[serde(default)]
        model_tier: crate::model::ModelTierRequirement,
    },
    CodeAct {
        contract: String,
        /// Имена стабов инструментов, доступных ИМЕННО этой программе —
        /// используется и статическим анализом (белый список, E7), и
        /// подсказкой модели. Пустой список — валиден (программе не
        /// нужны инструменты).
        #[serde(default)]
        tools: Vec<String>,
        #[serde(default)]
        model_tier: crate::model::ModelTierRequirement,
    },
    AgentStep {
        /// Контракт финального результата (`Finish.result`) — как у
        /// `CodeAct`, не отдельная форма на каждый `agent_step` в
        /// системе: сам цикл «рассуждение → действие → наблюдение»
        /// говорит фиксированным внутренним контрактом
        /// (`AgentTurnDecision`/`AgentVerdict`, `berimor-executors`),
        /// этот `contract` — только про то, что шаг ЗАПИШЕТ в состояние.
        contract: String,
        /// Жёсткий предел ходов (`executors.md` §5: «максимум ходов») —
        /// единственный реально принуждаемый лимит; бюджет токенов из
        /// той же строки документа честно не enforced — в системе
        /// нигде не считается использование токенов провайдером (тот
        /// же класс пробела, что `ProcessLimits.token_budget`, P6).
        max_turns: u32,
        /// Самокритика (`executors.md` §5): отрицательный вердикт по
        /// предложенному `Finish` не останавливает цикл — становится
        /// поводом для ещё одного хода.
        #[serde(default)]
        self_critique: bool,
        /// «Предложи — выполни — проверь» (`executors.md` §5):
        /// отдельный вердикт после каждого `Tool`-действия;
        /// отрицательный — терминальный исход цикла (эскалация), не
        /// повод для повтора.
        #[serde(default)]
        verify_actions: bool,
    },
    HumanGate {
        /// В декларации процесса — поле `reason` (`process-engine.md` §2);
        /// имя в Rust отражает, что значение почти всегда шаблон с `{{...}}`.
        #[serde(rename = "reason")]
        reason_template: String,
        /// Таймаут ожидания ответа человека (`process-engine.md` §5:
        /// «Таймаут ожидания человека — политика процесса»). `None` —
        /// ждать без таймаута (поведение Milestone 0/1, обратная
        /// совместимость с декларациями без этого поля). ROADMAP: P7.
        #[serde(
            default,
            rename = "timeout",
            deserialize_with = "crate::parser_support::deserialize_optional_duration_seconds"
        )]
        timeout_seconds: Option<u64>,
        /// Что делать по истечении таймаута — «падение шага, ветка по
        /// умолчанию или эскалация выше. Явно, не молчаливо» (там же).
        #[serde(default)]
        on_timeout: HumanGateTimeoutPolicy,
    },
    Checkpoint,
}

/// Политика на истечение таймаута `human_gate` (`process-engine.md` §5,
/// ROADMAP: P7) — дословно три исхода из документа.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum HumanGateTimeoutPolicy {
    /// Шаг падает — значение по умолчанию: без явного `on_timeout` в
    /// декларации таймаут обязан быть заметной ошибкой, не тихим
    /// продолжением по случайной ветке.
    #[default]
    Fail,
    /// Переход на конкретный следующий шаг, как если бы истёк срок —
    /// «ветка по умолчанию».
    Branch { to: String },
    /// Эскалация выше. Реальная маршрутизация (кому и как сообщить) —
    /// вне Process Engine (I5: ядро не имеет обязательных внешних
    /// зависимостей) — движок только фиксирует событие
    /// (`EventKind::HumanGateTimedOut`), дальнейшая обработка — забота
    /// вызывающего кода (диспетчер Actors, Фаза 7, или человек напрямую).
    Escalate,
}

#[derive(Debug, Clone, Serialize)]
pub struct Step {
    pub id: String,
    /// `flatten` — в декларации `type`/`contract`/... соседи `id` в одном
    /// объекте (`process-engine.md` §2), не вложенный под ключом `kind`.
    #[serde(flatten)]
    pub kind: StepKind,
}

/// Известные поля каждого тега `type` шага (аудит 1.9). serde не умеет
/// `deny_unknown_fields` ни на enum-вариантах, ни в сочетании с
/// `flatten`, поэтому неизвестное поле в декларации (`contrakt: ...`,
/// `maxturns: ...`) молча терялось. Список поддерживается ВМЕСТЕ с
/// вариантами `StepKind`: новое поле варианта = новая строка здесь,
/// иначе декларация с ним перестанет парситься (это и есть желаемая
/// fail-closed семантика — проверяется тестами на все golden-фикстуры).
fn known_step_keys(tag: &str) -> Option<&'static [&'static str]> {
    match tag {
        "sequential" => Some(&["type"]),
        "parallel" => Some(&["type", "branches"]),
        "loop" => Some(&["type", "condition"]),
        "branch" => Some(&["type", "on", "cases"]),
        "tool" => Some(&["type", "tool", "args"]),
        "llm_structured" => Some(&["type", "contract", "model_tier"]),
        "code_act" => Some(&["type", "contract", "tools", "model_tier"]),
        "agent_step" => Some(&[
            "type",
            "contract",
            "max_turns",
            "self_critique",
            "verify_actions",
        ]),
        "human_gate" => Some(&["type", "reason", "timeout", "on_timeout"]),
        "checkpoint" => Some(&["type"]),
        // Неизвестный тег — ошибку выдаст разбор `StepKind` ниже;
        // вайтлист здесь не сужаем, чтобы не подменять его диагностику.
        _ => None,
    }
}

impl<'de> Deserialize<'de> for Step {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct RawStep {
            id: String,
            #[serde(flatten)]
            rest: serde_json::Map<String, serde_json::Value>,
        }

        let raw = RawStep::deserialize(deserializer)?;
        // Вайтлист — ДО разбора `StepKind`: иначе разбор упал бы на
        // обязательном поле раньше, чем успел бы указать на опечатку
        // в соседнем ключе (аудит 1.9 — именно такая опечатка и опасна).
        if let Some(tag) = raw.rest.get("type").and_then(serde_json::Value::as_str) {
            if let Some(known) = known_step_keys(tag) {
                if let Some(extra) = raw.rest.keys().find(|key| !known.contains(&key.as_str())) {
                    return Err(serde::de::Error::custom(format!(
                        "неизвестное поле шага '{}': '{extra}' (известные: {})",
                        raw.id,
                        known.join(", ")
                    )));
                }
            }
        }
        let kind: StepKind = serde_json::from_value(serde_json::Value::Object(raw.rest))
            .map_err(serde::de::Error::custom)?;
        Ok(Step { id: raw.id, kind })
    }
}

/// Детерминированные прерыватели процесса — `process-engine.md` §4,
/// расширено `cost_budget`/`latency_budget_ms` (ADR-0011).
///
/// `timeout` и `token_budget` в декларации — человеко-читаемые
/// (`timeout: 10m`, `token_budget: 100k`, дословно из примера
/// `process-engine.md` §2), поэтому у обоих полей свой разбор строки с
/// суффиксом — см. [`parse_duration_seconds`]/[`parse_count`] в `parser.rs`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessLimits {
    pub max_steps: u32,
    #[serde(
        rename = "timeout",
        deserialize_with = "crate::parser_support::deserialize_duration_seconds"
    )]
    pub timeout_seconds: u64,
    #[serde(
        default,
        deserialize_with = "crate::parser_support::deserialize_optional_count"
    )]
    pub token_budget: Option<u64>,
    pub cost_budget: Option<f64>,
    pub latency_budget_ms: Option<u64>,
}

/// Декларативная модель процесса. Версионируется; хэш версии пишется в
/// каждое событие. Инстанс фиксирует версию при создании и не меняет её
/// сам по себе — миграция только через явную операцию (ADR-0012).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Process {
    /// В декларации — поле `process` (`process-engine.md` §2), не `name`:
    /// имя в Rust точнее отражает смысл (это имя процесса, а сам процесс —
    /// весь этот тип), но менять формат файла ради этого смысла нет причины.
    #[serde(rename = "process")]
    pub name: String,
    pub version: u32,
    pub steps: Vec<Step>,
    pub limits: ProcessLimits,
}

/// Декларативное описание изменения состояния. Патч не мутирует состояние
/// сам — применяет его только движок, атомарно (`process-engine.md` §3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Patch {
    pub step_id: String,
    pub changes: serde_json::Value,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Регрессионный тест аудита 1.9: опечатка в поле шага (`contrakt`)
    /// — ошибка разбора с именем поля, а не молчаливая потеря (раньше
    /// такой шаг получал `contract` = отсутствует → паника/дефолт позже).
    #[test]
    fn typo_in_step_field_is_a_parse_error() {
        let result = serde_json::from_str::<Step>(
            r#"{"id": "s1", "type": "llm_structured", "contrakt": "ClassificationOut"}"#,
        );
        let err = result.expect_err("опечатка в поле обязана отклоняться");
        assert!(err.to_string().contains("contrakt"), "{err}");
        assert!(err.to_string().contains("contract"), "{err}");
    }

    /// Опечатка в опциональном поле (`modle_tier`) — та же fail-closed
    /// семантика: раньше молча превращалась в `ModelTierRequirement::Any`,
    /// то есть в выбор ДРУГОГО класса модели без ведома автора декларации.
    #[test]
    fn typo_in_optional_step_field_is_a_parse_error() {
        let result = serde_json::from_str::<Step>(
            r#"{"id": "s1", "type": "llm_structured", "contract": "C", "modle_tier": "strong"}"#,
        );
        assert!(result.is_err(), "modle_tier обязан отклоняться");
    }

    /// Известные поля всех форм шагов по-прежнему парсятся — список
    /// `known_step_keys` не съедает легальные декларации.
    #[test]
    fn all_step_forms_still_parse() {
        for text in [
            r#"{"id": "a", "type": "sequential"}"#,
            r#"{"id": "b", "type": "parallel", "branches": ["x"]}"#,
            r#"{"id": "c", "type": "loop", "condition": "{{state.x}}"}"#,
            r#"{"id": "d", "type": "branch", "on": "{{state.x}}", "cases": {"1": "a"}}"#,
            r#"{"id": "e", "type": "tool", "tool": "t.x", "args": {}}"#,
            r#"{"id": "f", "type": "llm_structured", "contract": "C", "model_tier": "weak"}"#,
            r#"{"id": "g", "type": "code_act", "contract": "C", "tools": ["t"], "model_tier": "any"}"#,
            r#"{"id": "h", "type": "agent_step", "contract": "C", "max_turns": 3, "self_critique": true, "verify_actions": false}"#,
            r#"{"id": "i", "type": "human_gate", "reason": "r", "timeout": "5m", "on_timeout": {"action": "escalate"}}"#,
            r#"{"id": "j", "type": "checkpoint"}"#,
        ] {
            serde_json::from_str::<Step>(text).unwrap_or_else(|err| panic!("{text}: {err}"));
        }
    }

    /// Регрессионный тест аудита 1.9 (лимиты): `token_budjet` — ошибка,
    /// а не молчаливый `None` бюджета токенов.
    #[test]
    fn typo_in_limits_field_is_a_parse_error() {
        let result = serde_json::from_str::<Process>(
            r#"{"process": "p", "version": 1, "steps": [], "limits": {"max_steps": 5, "timeout": "1m", "token_budjet": "100k"}}"#,
        );
        assert!(result.is_err(), "token_budjet обязан отклоняться");
    }
}
