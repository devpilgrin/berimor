//! Model Pool: классы способностей, идентичность провайдера, запрос/ответ.
//!
//! Источник: `ideal-agent-architecture.md` §3.10, ADR-0010, ADR-0011.
//! ROADMAP: E3–E5.

use serde::{Deserialize, Serialize};

/// Режим подсказки формата ответа провайдеру (SGR-волна 0.30.0, issue #3,
/// спека `docs/rnd/sgr-wave-spec.md`). Значения — дословно строки конфигурации
/// `providers[].response_format`: `"none" | "json_object" | "json_schema" |
/// "grammar"`. Не задано в конфигурации — выводится из устаревшего
/// `json_object_response_format: bool` (обратная совместимость, см.
/// `berimor-cli::config::ProviderConfig::effective_response_format`).
///
/// Это ПОДСКАЗКА транспорту, не гарантия: валидирует ответ всё равно
/// Mediation (M2/M3), а не сервер и не клиент.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResponseFormat {
    /// Поле формата не отправляется вовсе (квирк провайдера, репорт
    /// 2026-08-08: LM Studio — 400 на `json_object`).
    None,
    /// `response_format: {"type": "json_object"}` (ollama-диалект:
    /// `format: "json"`).
    JsonObject,
    /// Constrained decoding по схеме контракта: `response_format:
    /// {"type": "json_schema", "json_schema": {"name": …, "schema": …,
    /// "strict": true}}` (ollama-диалект: `format` = объект схемы).
    JsonSchema,
    /// GBNF-грамматика из схемы. Конвертера схема→GBNF в дереве НЕТ
    /// (спека: «GBNF отложить с пометкой в доке») — для llama-server
    /// транспортируется как `json_schema` (он его принимает), для
    /// встроенного движка (E4) поведения не меняет.
    Grammar,
}

impl std::str::FromStr for ResponseFormat {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "none" => Ok(Self::None),
            "json_object" => Ok(Self::JsonObject),
            "json_schema" => Ok(Self::JsonSchema),
            "grammar" => Ok(Self::Grammar),
            other => Err(format!(
                "недопустимое значение response_format '{other}' — \
                 ожидается одно из: none, json_object, json_schema, grammar"
            )),
        }
    }
}

/// Присваивается кодом реестра моделей по офлайн-оценке на золотом наборе,
/// не самой моделью (ADR-0010: «присвоение класса — код, не самооценка»).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelTier {
    Weak,
    Medium,
    Strong,
}

/// Требование к классу модели, объявленное в шаге процесса
/// (`process-engine.md` §2, пример: `model_tier: any`). `Any` — не синоним
/// самого слабого класса, а «допуск не ограничен снизу»; чем ограничение
/// станет на практике для конкретного шага — решает Context Engine/Model
/// Pool при выборе провайдера (ADR-0011), не тип данных здесь.
///
/// `Any` — значение по умолчанию: последний шаг примера в `process-engine.md`
/// §2 (`answer`) вообще не указывает `model_tier` — отсутствие поля и
/// явное `any` неотличимы по смыслу, задавать оба способа как ошибку было
/// бы придиркой без содержания.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelTierRequirement {
    #[default]
    Any,
    Weak,
    Medium,
    Strong,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelIdentity {
    pub provider: String,
    pub model_id: String,
    pub tier: ModelTier,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionRequest {
    pub system_context: String,
    pub prompt: String,
    pub contract_name: Option<String>,
    /// Техдолг TD3.3 (`docs/audit-2026-07-31.md`): раньше HTTP-клиент
    /// включал `response_format: json_object` по факту `contract_name.is_some()`
    /// — но CodeAct тоже всегда передаёт `contract_name` (контракт
    /// РЕЗУЛЬТАТА для последующей Mediation), хотя от модели ожидается
    /// текст JS-программы, не JSON-объект. Явное поле, не выведенное из
    /// `contract_name`: `true` — `StructuredLlm`/`AgentStep` (ответ
    /// модели САМ — JSON по контракту), `false` — `CodeAct` (ответ —
    /// исходный текст программы; контракт применяется ПОЗЖЕ, к
    /// результату исполнения, не к самому ответу модели).
    pub expects_structured_output: bool,
    /// JSON Schema контракта ответа (SGR-волна 0.30.0, issue #3):
    /// проводится вызывающим (`StructuredLlm` — из реестра/конфиг-
    /// контракта, `AgentStep::decide_turn` — schemars `AgentTurnDecision`)
    /// и транспортируется провайдером при `ResponseFormat::JsonSchema`
    /// (constrained decoding: сервер сам держит форму, порядок полей в
    /// схеме = порядок генерации — связка с issue #4). `None` — схемы у
    /// вызывающего нет (CodeAct: ответ — программа, не JSON) или режим
    /// провайдера её не использует.
    #[serde(default)]
    pub json_schema: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionResponse {
    pub raw_text: String,
    pub model: ModelIdentity,
}

#[derive(Debug, thiserror::Error)]
pub enum ModelError {
    #[error("провайдер недоступен: {0}")]
    Unavailable(String),
    #[error("бюджет исчерпан: {0}")]
    BudgetExceeded(String),
}
