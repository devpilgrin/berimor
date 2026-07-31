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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Step {
    pub id: String,
    /// `flatten` — в декларации `type`/`contract`/... соседи `id` в одном
    /// объекте (`process-engine.md` §2), не вложенный под ключом `kind`.
    #[serde(flatten)]
    pub kind: StepKind,
}

/// Детерминированные прерыватели процесса — `process-engine.md` §4,
/// расширено `cost_budget`/`latency_budget_ms` (ADR-0011).
///
/// `timeout` и `token_budget` в декларации — человеко-читаемые
/// (`timeout: 10m`, `token_budget: 100k`, дословно из примера
/// `process-engine.md` §2), поэтому у обоих полей свой разбор строки с
/// суффиксом — см. [`parse_duration_seconds`]/[`parse_count`] в `parser.rs`.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
