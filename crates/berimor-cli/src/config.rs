//! Загрузка конфигурации агента.
//!
//! Источник: `docs/arch/stack.md` §2, `docs/arch/security-model.md` §3
//! (режимы подтверждений), `docs/arch/deployment.md` §10 (каналы
//! обновления). ROADMAP: F3.
//!
//! Формат — TOML: частичный файл переопределяет только указанные поля,
//! остальные берутся из [`Config::default`]. Путь конфигурации, доверенный
//! список и т.п. — не выбор модели, а явный аргумент/файл (I1, I2):
//! читать их из скрытых источников, кроме тех, что перечислены здесь, нельзя.

use berimor_types::{capability::ConfirmationMode, model::ModelTier};
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// Канал обновления агента (`deployment.md` §10) — локальная настройка
/// пользователя, не решается кодом автоматически.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateChannel {
    #[default]
    Stable,
    Beta,
    Canary,
}

/// Провайдер модели из конфигурации — регистрация в Model Pool (E3) и
/// параметры подключения HTTP-клиента (E5). Класс задаётся владельцем при
/// регистрации (паспорт модели, ADR-0010), не запрашивается у модели.
#[derive(Debug, Clone, Deserialize)]
pub struct ProviderConfig {
    /// Имя провайдера — ключ, связывающий запись пула и подключение.
    pub name: String,
    pub model_id: String,
    pub tier: ModelTier,
    /// Базовый URL OpenAI-совместимого API (без `/chat/completions`).
    /// Пуст для локального провайдера (задан `model_path`).
    #[serde(default)]
    pub base_url: String,
    /// Путь к GGUF-весам — признак ЛОКАЛЬНОГО провайдера (ROADMAP E4,
    /// ADR-0024: llama.cpp встроен в процесс, без сервера). `Some` —
    /// локальный инференс (`base_url`/`api_key_env` игнорируются),
    /// `None` — удалённый HTTP. Требует сборки с
    /// `--features local-inference`.
    #[serde(default)]
    pub model_path: Option<PathBuf>,
    /// ИМЯ переменной окружения с API-ключом — сам ключ в файле
    /// конфигурации не хранится (security-model.md §6: «нет секретов вне
    /// хранилища секретов»). Игнорируется при `auth = "oauth"`.
    pub api_key_env: Option<String>,
    /// ADR-0027, §20.25: `"oauth"` — access-токен берётся из OAuth-профиля
    /// реестра (`berimor login --provider ...`) через `oauth::access_token()`
    /// с прозрачным refresh; api_key_env не читается.
    #[serde(default)]
    pub auth: Option<String>,
    /// Имя OAuth-профиля в реестре ("claude"/"openai" — как у
    /// `berimor login --provider`). None — имя самого провайдера.
    #[serde(default)]
    pub oauth_profile: Option<String>,
    /// Явный opt-in на приватный endpoint (сетевой гейт S3) — для
    /// локальных серверов инференса и тестовых моков.
    #[serde(default)]
    pub allow_private_endpoint: bool,
    /// Стоимость из прайс-таблицы владельца (код-данные, ADR-0011).
    #[serde(default)]
    pub cost_per_1k_tokens: Option<f64>,
    /// Явная температура запросов; None — 0.0 (воспроизводимость).
    /// Часть моделей принимает только temperature=1 (Kimi k3) — пресет
    /// kimi задаёт её (§20.14, репорт 2026-08-03).
    #[serde(default)]
    pub temperature: Option<f32>,
    /// `response_format: {"type": "json_object"}` в запросе — большинство
    /// OpenAI-совместимых серверов его принимают, но не все (репорт
    /// 2026-08-08: LM Studio отвечает 400 «response_format.type must be
    /// 'json_schema' or 'text'» — весь чат структурного вывода падал на
    /// каждом ходе). `false` — клиент не шлёт `response_format` вовсе,
    /// полагаясь на инструкцию формата в самом промпте (как уже делает
    /// CodeAct); пресет lmstudio задаёт `false`.
    #[serde(default = "default_true")]
    pub json_object_response_format: bool,
    /// Режим подсказки формата ответа (SGR-волна 0.30.0, issue #3):
    /// "none" | "json_object" | "json_schema" | "grammar". Не задано —
    /// выводится из `json_object_response_format` (обратная
    /// совместимость). `json_schema` — constrained decoding: схема
    /// контракта уходит в поле запроса (OpenAI-диалект) или в `format`
    /// (ollama), порядок полей схемы становится порядком генерации —
    /// связка с полями-обоснованиями (issue #4).
    #[serde(default)]
    pub response_format: Option<String>,
    /// Потолок одного HTTP-вызова провайдеру в секундах; `None` —
    /// `berimor_model_pool::http_provider::DEFAULT_REQUEST_TIMEOUT_SECS`
    /// (150с, поднято ×5 директивой 2026-08-08 — локальные reasoning-
    /// модели легко превышали прежние 30с уже на первом ходе агентного
    /// цикла, ловя транспортный таймаут вдобавок ретраящийся 4 раза).
    #[serde(default)]
    pub request_timeout_secs: Option<u64>,
    /// Контекст локального GGUF (только с model_path; 0.35.2): `None` —
    /// 8192. Поднято с 4096 по репорту 2026-08-16: большой структурный
    /// ответ упирался в потолок контекста и обрывался («EOF while
    /// parsing» на эскалации). Второй рубеж — EOF-ремонт в медиации.
    #[serde(default)]
    pub local_ctx_tokens: Option<u32>,
}

/// Настройки свободного агентного цикла (0.34.0).
#[derive(Debug, Clone, Deserialize)]
pub struct AgentConfig {
    /// Ходов на одно сообщение чата. Дефолт поднят 12 → 32: со стражем
    /// зацикливания высокий потолок безопасен, а анализ проекта за
    /// десяток разных чтений не должен умирать по лимиту.
    #[serde(default = "default_agent_max_turns")]
    pub max_turns: u32,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            max_turns: default_agent_max_turns(),
        }
    }
}

fn default_agent_max_turns() -> u32 {
    32
}

fn default_true() -> bool {
    true
}

impl ProviderConfig {
    /// Эффективный режим формата: явный `response_format` (валидируется
    /// строкой конфига — опечатка = ошибка загрузки, не молчаливый
    /// даунгрейд), иначе вывод из устаревшего bool (true → json_object,
    /// false → none).
    pub fn effective_response_format(
        &self,
    ) -> Result<berimor_types::model::ResponseFormat, ConfigError> {
        match &self.response_format {
            Some(value) => value
                .parse::<berimor_types::model::ResponseFormat>()
                .map_err(|reason| ConfigError::InvalidProviderValue {
                    provider: self.name.clone(),
                    field: "response_format".into(),
                    reason,
                }),
            None => Ok(if self.json_object_response_format {
                berimor_types::model::ResponseFormat::JsonObject
            } else {
                berimor_types::model::ResponseFormat::None
            }),
        }
    }
}

/// Заглушка инструмента для `berimor run`: детерминированный ответ на
/// вызов по имени. Реальные интеграции инструментов — MCP (Фаза 8, T1);
/// до них ToolOnly исполняется против объявленных здесь ответов, что и
/// позволяет прогонять процессы end-to-end. `mutates` — часть декларации
/// политики инструмента (S4): read-only заглушки не требуют подтверждения
/// в режиме smart.
#[derive(Debug, Clone, Deserialize)]
pub struct ToolStub {
    pub tool: String,
    #[serde(default)]
    pub mutates: bool,
    pub response: serde_json::Value,
}

/// Контракт структурного вывода из конфигурации (`[[contracts]]`, спека
/// docs/rnd/config-contracts-spec.md, 2026-08-14): оператор объявляет
/// JSON Schema и использует имя в шагах `llm_structured`/`codeact`
/// наравне с кодовыми контрактами (реестр E2). Схема — inline (`schema`)
/// или файлом (`schema_path`, относительно каталога файла конфигурации);
/// формы взаимоисключающие, одна из двух обязательна. После загрузки
/// (`load`) `schema_path` всегда разрешён в `schema` — дальше по
/// конвейеру ходит один источник схемы.
#[derive(Debug, Clone, Deserialize)]
pub struct ContractConfig {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub schema: Option<String>,
    #[serde(default)]
    pub schema_path: Option<PathBuf>,
}

/// Настройки слоёв памяти в Context Engine (`MemoryContextBuilder`).
/// Оба поля опциональны — без `skills_dir` слой Skills просто пуст, без
/// изменения остального поведения (обратная совместимость с конфигом,
/// в котором секции `[memory]` нет вовсе).
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct MemoryConfig {
    /// Директория с файлами навыков (`berimor_memory::procedural`
    /// формат — YAML-фронтматтер + тело). Не задана — слой Skills пуст.
    pub skills_dir: Option<PathBuf>,
    /// Верхняя граница числа сессий в слое Session за один запрос.
    pub session_search_limit: usize,
    /// BR-04 (полевой тест 2026-08-14): подмешивать ли в контекст
    /// модели результаты ПРОШЛЫХ прогонов из той же рабочей директории
    /// (слой Session). Default `false`: неявная передача сведений за
    /// границу задачи недопустима для контуров с ограничениями на
    /// обработку данных; включение — осознанное решение оператора.
    /// При `false` слой пуст независимо от `session_search_limit`.
    #[serde(default)]
    pub session_context: bool,
    /// Слой графа сущностей в контексте (ROADMAP §20.5, memory-model.md
    /// §4: «включается профилем процесса, не глобально»). Граф читается
    /// из того же журнала SQLite (`EntityGraphStore`), наполняется
    /// внешними процессами — ядро в `berimor run` его только читает.
    pub entity_graph: bool,
    /// Записной путь памяти (memory-model.md §2/§4): после завершения
    /// процесса модель извлекает факты из финального состояния
    /// (контракт FactProposalBatch) и они проходят конвейер «Mediation →
    /// дедупликация/конфликт → запись». Default `false` — запись в
    /// память это доверенная граница, включается осознанно.
    #[serde(default)]
    pub fact_extraction: bool,
    /// Семантическая близость фактов на эмбеддингах (ROADMAP §20.23):
    /// при `true` И сборке с `--features embeddings` — на записи:
    /// дедупликация использует `VectorSimilarity` с fastembed
    /// (`BAAI/bge-m3`, 1024-мерный), новые факты сохраняются с
    /// эмбеддингом для sqlite-vec; на чтении (prompt-next-wave.md задача
    /// 1): слой `Facts` контекста ищет релевантные факты через
    /// `SemanticStore::hybrid_search`. Default `false`: поведение прежнее
    /// — дедупликация только по точному хэшу, слой Facts отсутствует,
    /// модель не скачивается.
    #[serde(default)]
    pub embeddings: bool,
    /// Запись в память ИНСТРУМЕНТОМ `memory.save` (волна C8, spec
    /// builtin-tools-waves): как и fact_extraction — доверенная граница,
    /// включается осознанно. Default `false`: инструмент отвечает
    /// говорящей ошибкой «запись отключена конфигом».
    #[serde(default)]
    pub tool_writes: bool,
    /// Верхняя граница числа фактов в слое `Facts` за один запрос
    /// (аналог `session_search_limit`).
    pub facts_search_limit: usize,
}

/// `berimor serve` (prompt-next-wave.md задача 2): HTTP-сервис поверх
/// существующих операций CLI. Токен — ИМЯ переменной окружения, не
/// значение (тот же принцип, что `ProviderConfig::api_key_env`,
/// `security-model.md` §6: секреты не хранятся в файле конфигурации).
/// `token_env: None` — `berimor serve` отказывается стартовать (I2:
/// исполнение процессов по сети не бывает анонимным по умолчанию).
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ServeConfig {
    pub port: u16,
    pub token_env: Option<String>,
}

impl Default for ServeConfig {
    fn default() -> Self {
        Self {
            port: 8787,
            token_env: None,
        }
    }
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            skills_dir: None,
            session_search_limit: 5,
            session_context: false,
            entity_graph: false,
            fact_extraction: false,
            embeddings: false,
            tool_writes: false,
            facts_search_limit: 5,
        }
    }
}

/// Внешний сервер инструментов по MCP (T1) — оператор сам прописывает его
/// здесь; доверие к серверу — факт присутствия в конфиге, как и у
/// `tool_stubs`. Установка/доверенный список плагинов (D6) — отдельный,
/// пока не реализованный процесс, эта секция его не подменяет.
#[derive(Debug, Clone, Deserialize)]
pub struct McpServerConfig {
    /// Имя сервера — только для сообщений об ошибках и разрешения
    /// конфликтов имён инструментов между серверами, в протоколе не
    /// участвует.
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Путь к файлу журнала SQLite (`berimor-storage::SqliteEventLog`).
    pub storage_path: PathBuf,
    /// `smart` — интерактивный режим по умолчанию (security-model.md §3).
    pub confirmation_mode: ConfirmationMode,
    pub update_channel: UpdateChannel,
    pub providers: Vec<ProviderConfig>,
    pub tool_stubs: Vec<ToolStub>,
    /// Контракты из конфигурации (спека config-contracts): после `load`
    /// каждый гарантированно несёт inline-схему и прошёл валидацию
    /// (уникальность имени, отсутствие совпадения с кодовыми
    /// контрактами, компиляция JSON Schema).
    pub contracts: Vec<ContractConfig>,
    pub memory: MemoryConfig,
    pub mcp_servers: Vec<McpServerConfig>,
    /// Имена переменных окружения, чьи ЗНАЧЕНИЯ — секреты этого запуска
    /// (S5, mediation.md §4.3): регистрируются в маскировщике и заменяются
    /// алиасом на всех границах данных. Ключи API провайдеров
    /// (`providers[].api_key_env`) регистрируются автоматически, здесь их
    /// дублировать не нужно. Сами значения в конфигурации не хранятся
    /// никогда (security-model.md §6).
    #[serde(default)]
    pub secret_envs: Vec<String>,
    /// Инструменты, разрешённые к мутирующим действиям БЕЗ вопроса
    /// (0.14.0, директива «не ебать пользователю мозги»): глобальные и
    /// проектные разрешения складываются (union). Deny-статика и jail
    /// выше — разрешение снимает ВОПРОС, не запрет. Плюс allow-лист
    /// области (`.berimor/allow`, пишет модал «для проекта»).
    #[serde(default)]
    pub auto_confirm: Vec<String>,
    /// Свободный агентный цикл (0.34.0): бюджет ходов на сообщение чата.
    /// Страж зацикливания (повтор действия подряд) отдельно и раньше —
    /// этот лимит защищает от длинной работы, не от бессмысленной.
    #[serde(default)]
    pub agent: AgentConfig,
    #[serde(default)]
    pub serve: ServeConfig,
    /// Интерфейс (2026-08-09): `[ui] locale = "en"` — локаль TUI из 8
    /// (см. `i18n::Locale`); не задана — локаль окружения, затем ru.
    #[serde(default)]
    pub ui: UiConfig,
}

/// Секция `[ui]` конфигурации: настройки интерфейса (пока — локаль).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct UiConfig {
    pub locale: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            storage_path: PathBuf::from("./berimor.db"),
            confirmation_mode: ConfirmationMode::Smart,
            update_channel: UpdateChannel::Stable,
            providers: Vec::new(),
            tool_stubs: Vec::new(),
            contracts: Vec::new(),
            memory: MemoryConfig::default(),
            mcp_servers: Vec::new(),
            secret_envs: Vec::new(),
            auto_confirm: Vec::new(),
            agent: AgentConfig::default(),
            serve: ServeConfig::default(),
            ui: UiConfig::default(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("не удалось прочитать файл конфигурации {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("не удалось разобрать файл конфигурации {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("не удалось прочитать файл схемы контракта '{contract}' ({path}): {source}")]
    ContractSchemaRead {
        contract: String,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// Невалидное объявление `[[contracts]]` (спека config-contracts
    /// п.2): дубликат имени, совпадение с кодовым контрактом, битая
    /// схема — любая ошибка есть ошибка загрузки конфигурации.
    #[error("контракт '{contract}' из конфигурации: {reason}")]
    Contract { contract: String, reason: String },
    /// Невалидное значение поля провайдера (SGR 0.30.0: опечатка в
    /// `response_format` — ошибка загрузки, не молчаливый даунгрейд).
    #[error("провайдер '{provider}', поле '{field}': {reason}")]
    InvalidProviderValue {
        provider: String,
        field: String,
        reason: String,
    },
}

// --- Слоистая конфигурация (§20.12) ------------------------------------

/// Директива 2026-08-09: служебные файлы проекта (конфиг, журнал,
/// allow-лист) не должны захламлять корень — все новые складываются
/// под `.berimor/` (тот же каталог, что уже занят `skills/`/`agents/`,
/// §20.16/20.17).
const PROJECT_STATE_DIR: &str = ".berimor";

/// Чистая логика выбора пути — без обращения к CWD внутри, ради теста
/// без гонок между параллельными тестами (`std::env::set_current_dir`
/// в многопоточном тестовом бинаре — общий процесс, отдельный `#[test]`
/// не может безопасно менять глобальный CWD). Уже существующий легаси-
/// файл в корне побеждает: директива «старые проекты не трогать» — ни
/// переноса, ни молчаливой потери уже накопленного состояния (пропавший
/// провайдер/журнал будет выглядеть как баг, не как миграция).
fn prefer_legacy_or_new(workspace: &Path, legacy_relative: &str, new_relative: &str) -> PathBuf {
    let legacy = workspace.join(legacy_relative);
    if legacy.is_file() {
        legacy
    } else {
        workspace.join(PROJECT_STATE_DIR).join(new_relative)
    }
}

fn current_dir_or_dot() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

/// Путь локальной конфигурации по умолчанию (слой проекта поверх
/// глобального, §20.12): `.berimor/config.toml` для новых проектов;
/// уже существующий `./berimor.toml` (до 2026-08-09) — используется
/// как есть.
pub fn default_config_path() -> PathBuf {
    prefer_legacy_or_new(&current_dir_or_dot(), "berimor.toml", "config.toml")
}

/// Путь SQLite-журнала по умолчанию — тот же принцип обратной
/// совместимости.
fn default_storage_path() -> PathBuf {
    prefer_legacy_or_new(&current_dir_or_dot(), "berimor.db", "berimor.db")
}

/// Промежуточная форма файла конфигурации: скаляры опциональны, чтобы
/// слияние отличало «задано в файле» от «умолчание» (сырой `Config` с
/// serde-default этого не позволяет — слой-источник настроек потерялся
/// бы). Коллекции и так сливаются поимённо.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct PartialConfig {
    pub storage_path: Option<PathBuf>,
    pub confirmation_mode: Option<ConfirmationMode>,
    pub update_channel: Option<UpdateChannel>,
    pub providers: Vec<ProviderConfig>,
    pub tool_stubs: Vec<ToolStub>,
    pub contracts: Vec<ContractConfig>,
    pub memory: Option<MemoryConfig>,
    pub mcp_servers: Vec<McpServerConfig>,
    pub secret_envs: Vec<String>,
    #[serde(default)]
    pub auto_confirm: Vec<String>,
    pub agent: Option<AgentConfig>,
    pub serve: Option<ServeConfig>,
    pub ui: Option<UiConfig>,
}

impl PartialConfig {
    fn load_file(path: &Path) -> Result<Option<Self>, ConfigError> {
        match std::fs::read_to_string(path) {
            Ok(contents) => {
                let mut partial: Self =
                    toml::from_str(&contents).map_err(|source| ConfigError::Parse {
                        path: path.to_path_buf(),
                        source,
                    })?;
                // `schema_path` разрешается в inline-схему сразу, пока
                // известен каталог ЭТОГО файла (слои грузятся раздельно,
                // у каждого свой базовый каталог), затем объявления
                // валидируются — битый контракт = ошибка загрузки, не
                // отложенный сбой шага (спека config-contracts п.2).
                resolve_contract_schemas(&mut partial.contracts, path)?;
                validate_contracts(&partial.contracts)?;
                Ok(Some(partial))
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(ConfigError::Read {
                path: path.to_path_buf(),
                source,
            }),
        }
    }
}

/// `schema_path` → inline: файл схемы читается относительно каталога
/// файла конфигурации, в котором объявлен контракт. После разрешения
/// `schema_path` обнуляется — дальше по конвейеру один источник схемы.
fn resolve_contract_schemas(
    contracts: &mut [ContractConfig],
    config_path: &Path,
) -> Result<(), ConfigError> {
    let base = config_path.parent().unwrap_or_else(|| Path::new("."));
    for contract in contracts {
        let Some(schema_path) = contract.schema_path.take() else {
            continue;
        };
        // Конфликт форм ловим ДО перезаписи inline-схемы содержимым
        // файла — иначе schema_path молча побеждал бы schema.
        if contract.schema.is_some() {
            return Err(ConfigError::Contract {
                contract: contract.name.clone(),
                reason: "schema и schema_path взаимоисключающие — оставьте одну форму".into(),
            });
        }
        let full = base.join(&schema_path);
        let contents =
            std::fs::read_to_string(&full).map_err(|source| ConfigError::ContractSchemaRead {
                contract: contract.name.clone(),
                path: full.clone(),
                source,
            })?;
        contract.schema = Some(contents);
    }
    Ok(())
}

/// Валидация `[[contracts]]` при загрузке (спека config-contracts п.2):
/// имя уникально и НЕ совпадает с кодовыми контрактами (реестр E2,
/// `structured_llm`) — иначе поведение `berimor run` зависело бы от
/// порядка поиска; ровно одна форма схемы (inline ИЛИ файл); схема
/// парсится как JSON и компилируется `jsonschema::validator_for`.
fn validate_contracts(contracts: &[ContractConfig]) -> Result<(), ConfigError> {
    let mut seen = std::collections::HashSet::new();
    for contract in contracts {
        if !seen.insert(contract.name.clone()) {
            return Err(ConfigError::Contract {
                contract: contract.name.clone(),
                reason: "имя дублируется в конфигурации".into(),
            });
        }
        if berimor_executors::structured_llm::find_contract(&contract.name).is_some() {
            return Err(ConfigError::Contract {
                contract: contract.name.clone(),
                reason: "имя совпадает с кодовым контрактом (реестр E2) — переименуйте объявление"
                    .into(),
            });
        }
        match (&contract.schema, &contract.schema_path) {
            (Some(_), Some(_)) => {
                return Err(ConfigError::Contract {
                    contract: contract.name.clone(),
                    reason: "schema и schema_path взаимоисключающие — оставьте одну форму".into(),
                })
            }
            (None, None) => {
                return Err(ConfigError::Contract {
                    contract: contract.name.clone(),
                    reason: "нужна schema (inline JSON Schema) или schema_path (файл со схемой)"
                        .into(),
                })
            }
            (Some(schema), None) => {
                berimor_executors::structured_llm::ConfigContract::new(
                    contract.name.clone(),
                    contract.description.clone(),
                    schema,
                )
                .map_err(|err| ConfigError::Contract {
                    contract: contract.name.clone(),
                    reason: err.to_string(),
                })?;
            }
            // load_file разрешает schema_path в inline до валидации;
            // недозревший путь сюда попасть не должен.
            (None, Some(_)) => {
                return Err(ConfigError::Contract {
                    contract: contract.name.clone(),
                    reason: "schema_path не разрешён в inline-схему (внутренняя ошибка загрузки)"
                        .into(),
                })
            }
        }
    }
    Ok(())
}

/// Глобальная директория berimor: `$XDG_CONFIG_HOME/berimor` или
/// `~/.config/berimor`. Директория — не файл: рядом с `config.toml`
/// лежит `secrets.env` (см. ниже).
pub fn global_dir() -> Option<PathBuf> {
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        return Some(PathBuf::from(xdg).join("berimor"));
    }
    if let Some(home) = std::env::var_os("HOME") {
        return Some(PathBuf::from(home).join(".config").join("berimor"));
    }
    // Windows: HOME обычно не задан (CI windows-latest 2026-08-04 —
    // global_dir возвращал None, chat-history молча не писалась, тесты
    // падали с 0 записей). dirs::config_dir() — %APPDATA% и аналоги.
    dirs::config_dir().map(|dir| dir.join("berimor"))
}

pub fn global_config_path() -> Option<PathBuf> {
    global_dir().map(|dir| dir.join("config.toml"))
}

pub fn secrets_env_path() -> Option<PathBuf> {
    global_dir().map(|dir| dir.join("secrets.env"))
}

/// Есть ли ХОТЬ ОДИН файл конфигурации (глобальный или локальный) —
/// сигнал first-run для мастера настройки (`berimor setup`).
pub fn any_config_present(explicit_path: Option<&Path>) -> bool {
    let global = global_config_path().is_some_and(|p| p.is_file());
    let local = explicit_path
        .map(|p| p.is_file())
        .unwrap_or_else(|| default_config_path().is_file());
    global || local
}

/// Слияние слоёв (§20.12): локальный переопределяет глобальный.
/// Скаляры — локальный, если задан явно, иначе глобальный, иначе
/// умолчание `Config`. Провайдеры/заглушки/MCP — объединение по имени,
/// при совпадении имени побеждает локальный. Секция `[memory]`
/// заменяется целиком (её поля не разделяются по слоям — осознанное
/// упрощение, задокументировано здесь). `secret_envs` — объединение.
pub fn merge(global: PartialConfig, local: PartialConfig) -> Config {
    fn merge_named<T, K: Eq + std::hash::Hash>(
        global: Vec<T>,
        local: Vec<T>,
        key: impl Fn(&T) -> K,
    ) -> Vec<T> {
        let local_keys: std::collections::HashSet<K> = local.iter().map(&key).collect();
        let mut merged: Vec<T> = global
            .into_iter()
            .filter(|item| !local_keys.contains(&key(item)))
            .collect();
        merged.extend(local);
        merged
    }

    let defaults = Config::default();
    let mut secret_envs = global.secret_envs;
    for name in local.secret_envs {
        if !secret_envs.contains(&name) {
            secret_envs.push(name);
        }
    }
    let mut auto_confirm = global.auto_confirm;
    for name in local.auto_confirm {
        if !auto_confirm.contains(&name) {
            auto_confirm.push(name);
        }
    }
    Config {
        storage_path: local
            .storage_path
            .or(global.storage_path)
            .unwrap_or_else(default_storage_path),
        confirmation_mode: local
            .confirmation_mode
            .or(global.confirmation_mode)
            .unwrap_or(defaults.confirmation_mode),
        update_channel: local
            .update_channel
            .or(global.update_channel)
            .unwrap_or(defaults.update_channel),
        providers: merge_named(global.providers, local.providers, |p| p.name.clone()),
        tool_stubs: merge_named(global.tool_stubs, local.tool_stubs, |s| s.tool.clone()),
        contracts: merge_named(global.contracts, local.contracts, |c| c.name.clone()),
        memory: local.memory.or(global.memory).unwrap_or(defaults.memory),
        mcp_servers: merge_named(global.mcp_servers, local.mcp_servers, |s| s.name.clone()),
        secret_envs,
        auto_confirm,
        serve: local.serve.or(global.serve).unwrap_or(defaults.serve),
        // `[agent]` — секция целиком, локальный слой сильнее (как [ui]).
        agent: local.agent.or(global.agent).unwrap_or_default(),
        // `[ui]` — как `[memory]`: секция заменяется целиком, локальный
        // слой сильнее (осознанное упрощение, задокументировано здесь).
        ui: local.ui.or(global.ui).unwrap_or(defaults.ui),
    }
}

/// Путь allow-листа: `.berimor/allow` для новых проектов; уже
/// существующий легаси `./.berimor-allow` (до 2026-08-09) — используется
/// как есть (см. `prefer_legacy_or_new`).
fn project_allow_path(workspace: &Path) -> PathBuf {
    prefer_legacy_or_new(workspace, ".berimor-allow", "allow")
}

/// Проектные разрешения на мутации: по одному имени инструмента на
/// строку (`#` — комментарии). Пишет модал подтверждения («разрешить
/// для проекта»), читается при сборке бандла. Файл, а не ключ в TOML:
/// дописывать строку честнее, чем переписывать пользовательский конфиг
/// сериализатором.
pub fn load_project_allow(workspace: &std::path::Path) -> Vec<String> {
    let Ok(contents) = std::fs::read_to_string(project_allow_path(workspace)) else {
        return Vec::new();
    };
    contents
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(str::to_string)
        .collect()
}

/// Дописывает разрешение в allow-лист области (идемпотентно).
pub fn append_project_allow(workspace: &std::path::Path, tool: &str) -> std::io::Result<()> {
    if load_project_allow(workspace).iter().any(|t| t == tool) {
        return Ok(());
    }
    use std::io::Write as _;
    let path = project_allow_path(workspace);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?; // .berimor/ может ещё не существовать
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    writeln!(file, "{tool}")
}

/// Секреты глобального уровня: `secrets.env` рядом с глобальным
/// конфигом, формат `KEY=value` (строки `#` — комментарии). Пишет
/// мастер настройки с правами 0600. Переменная, УЖЕ заданная в
/// окружении процесса, не переопределяется — явное окружение сильнее
/// файла (стандартный приоритет env над dotenv).
pub fn load_secrets_env(path: &Path) {
    let Ok(contents) = std::fs::read_to_string(path) else {
        return;
    };
    for (name, value) in parse_secrets_env(&contents) {
        if std::env::var_os(&name).is_none() {
            std::env::set_var(&name, &value);
        }
    }
}

/// Чистый разбор `KEY=value` — отдельно от применения, чтобы
/// тестировался без мутации окружения процесса.
pub fn parse_secrets_env(contents: &str) -> Vec<(String, String)> {
    contents
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            let (name, value) = line.split_once('=')?;
            let name = name.trim();
            if name.is_empty() {
                return None;
            }
            Some((name.to_string(), value.trim().to_string()))
        })
        .collect()
}

/// Загружает конфигурацию по слоям (§20.12): глобальный
/// (`~/.config/berimor/config.toml`) ← локальный (явный `--config` или
/// `./berimor.toml`) — локальный переопределяет. Отсутствие обоих
/// файлов — не ошибка: значения [`Config::default`] (первый запуск до
/// мастера настройки); нечитаемый или невалидный СУЩЕСТВУЮЩИЙ файл —
/// ошибка: молчаливо игнорировать испорченную конфигурацию нельзя (I2).
/// Перед слиянием подхватывается глобальный `secrets.env`.
pub fn load(explicit_path: Option<&Path>) -> Result<Config, ConfigError> {
    if let Some(secrets) = secrets_env_path() {
        load_secrets_env(&secrets);
    }
    let global = match global_config_path() {
        Some(path) => PartialConfig::load_file(&path)?,
        None => None,
    };
    let local_path = explicit_path
        .map(Path::to_path_buf)
        .unwrap_or_else(default_config_path);
    // Находка 3.10 аудита: ЯВНО указанный путь, которого нет, — ошибка,
    // не молчаливая подмена дефолтами (опечатка в пути меняла бы
    // security-режим незаметно: smart вместо off, потеря провайдеров).
    // Дефолтный ./berimor.toml по-прежнему опционален.
    if explicit_path.is_some() && !local_path.is_file() {
        return Err(ConfigError::Read {
            path: local_path.clone(),
            source: std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "явно указанный --config не существует (проверьте путь — молчаливая подмена дефолтами отключена)",
            ),
        });
    }
    let local = PartialConfig::load_file(&local_path)?;
    let config = merge(global.unwrap_or_default(), local.unwrap_or_default());
    // .berimor/ может ещё не существовать для нового проекта (директива
    // 2026-08-09) — SqliteEventLog::open не создаёт родительские
    // директории сама, откроется с ошибкой ДО этой правки. Ошибка здесь
    // намеренно не всплывает: не она откроет журнал, это сделает
    // storage::open чуть позже с говорящим сообщением — эта попытка
    // просто заранее готовит директорию, best-effort.
    if let Some(parent) = config.storage_path.parent() {
        if !parent.as_os_str().is_empty() {
            let _ = std::fs::create_dir_all(parent);
        }
    }
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// BR-04 (полевой тест 2026-08-14): слой Session в контексте —
    /// по умолчанию выключен, включается явным флагом.
    #[test]
    fn session_context_defaults_off_and_opt_in() {
        let plain: Config = toml::from_str("").unwrap();
        assert!(
            !plain.memory.session_context,
            "по умолчанию посторонние прогоны не подмешиваются"
        );
        let opted: Config = toml::from_str("[memory]\nsession_context = true\n").unwrap();
        assert!(opted.memory.session_context);
        // Лимит читается независимо — гейтится уже в точке сборки.
        assert_eq!(opted.memory.session_search_limit, 5);
    }

    /// Находка 3.10 аудита: явный --config, которого нет, — ошибка, не
    /// молчаливые дефолты (опечатка в пути ≠ смена security-режима).
    #[test]
    fn explicit_missing_config_is_error_not_silent_defaults() {
        let missing = std::path::Path::new("/nonexistent/berimor-3-10.toml");
        let result = load(Some(missing));
        assert!(result.is_err(), "явный несуществующий --config — ошибка");
        // Дефолтный путь по-прежнему опционален.
        let _ = load(None);
    }

    /// E4: локальный провайдер объявляется `model_path` без `base_url` —
    /// поле URL обязано иметь дефолт, иначе конфигурация локального
    /// инференса потребовала бы бессмысленный URL.
    #[test]
    fn local_provider_needs_no_base_url() {
        let text = r#"
[[providers]]
name = "llama-local"
model_id = "qwen3-4b-q4"
tier = "weak"
model_path = "/models/qwen3-4b-q4_k_m.gguf"
"#;
        let config: Config = toml::from_str(text).unwrap();
        let provider = &config.providers[0];
        assert_eq!(
            provider.model_path.as_deref(),
            Some(std::path::Path::new("/models/qwen3-4b-q4_k_m.gguf"))
        );
        assert!(provider.base_url.is_empty());
        assert!(provider.api_key_env.is_none());
    }

    /// Удалённый провайдер без `model_path` — прежняя форма, локальный
    /// признак не должен включаться случайно.
    #[test]
    fn remote_provider_has_no_model_path() {
        let text = r#"
[[providers]]
name = "openai"
model_id = "gpt-4o-mini"
tier = "medium"
base_url = "https://api.openai.com/v1"
"#;
        let config: Config = toml::from_str(text).unwrap();
        assert!(config.providers[0].model_path.is_none());
    }

    #[test]
    fn default_config_has_documented_values() {
        let config = Config::default();
        assert_eq!(config.storage_path, PathBuf::from("./berimor.db"));
        assert_eq!(config.confirmation_mode, ConfirmationMode::Smart);
        assert_eq!(config.update_channel, UpdateChannel::Stable);
        // §20.23: эмбеддинги — opt-in и по конфигурации, и по feature.
        assert!(!config.memory.embeddings);
    }

    /// §20.23: `[memory] embeddings = true` разбирается из TOML;
    /// отсутствие поля — прежнее поведение (false), обратная
    /// совместимость конфигов без этого ключа.
    #[test]
    fn memory_embeddings_flag_parses_and_defaults_off() {
        let on: Config =
            toml::from_str("[memory]\nembeddings = true\nfact_extraction = true\n").unwrap();
        assert!(on.memory.embeddings);
        let off: Config = toml::from_str("[memory]\nfact_extraction = true\n").unwrap();
        assert!(!off.memory.embeddings);
    }

    #[test]
    fn missing_explicit_config_is_error_missing_default_falls_back() {
        // Контракт 3.10: ЯВНЫЙ путь — обязан существовать (опечатка не
        // должна молча менять security-режим); ДЕФОЛТНЫЙ — опционален.
        assert!(load(Some(Path::new("/nonexistent/path/does-not-exist.toml"))).is_err());
        // load(None) с отсутствующим ./berimor.toml — дефолты (проверено
        // в explicit_missing_config_is_error_not_silent_defaults).
    }

    #[test]
    fn partial_file_overrides_only_specified_fields() {
        let dir = std::env::temp_dir().join(format!("berimor-config-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("partial.toml");
        std::fs::write(&path, "update_channel = \"beta\"\n").unwrap();

        let config = load(Some(&path)).unwrap();

        assert_eq!(config.update_channel, UpdateChannel::Beta);
        // Не Config::default().storage_path (2026-08-09): дефолт теперь
        // I/O-зависимый (легаси-путь в корне побеждает, если уже
        // существует) — эталон тот же самый resolver, не голая константа.
        assert_eq!(
            config.storage_path,
            default_storage_path(),
            "неуказанное поле должно остаться значением по умолчанию"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn full_file_overrides_all_fields() {
        let dir =
            std::env::temp_dir().join(format!("berimor-config-test-full-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("full.toml");
        std::fs::write(
            &path,
            "storage_path = \"/tmp/custom.db\"\nconfirmation_mode = \"manual\"\nupdate_channel = \"canary\"\n",
        )
        .unwrap();

        let config = load(Some(&path)).unwrap();

        assert_eq!(config.storage_path, PathBuf::from("/tmp/custom.db"));
        assert_eq!(config.confirmation_mode, ConfirmationMode::Manual);
        assert_eq!(config.update_channel, UpdateChannel::Canary);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn invalid_toml_in_existing_file_is_an_error_not_silently_ignored() {
        let dir = std::env::temp_dir().join(format!(
            "berimor-config-test-invalid-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("broken.toml");
        std::fs::write(&path, "this is not valid toml =====").unwrap();

        let result = load(Some(&path));

        assert!(
            matches!(result, Err(ConfigError::Parse { .. })),
            "испорченный существующий файл конфигурации не должен молчаливо игнорироваться"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Директива 2026-08-09: «служебные файлы должны лежать в
    /// .berimor/». Новый проект (ни легаси-файла, ни `.berimor/`) —
    /// путь под `.berimor/`. `prefer_legacy_or_new` параметризован
    /// workspace ради теста без гонок по глобальному CWD.
    #[test]
    fn prefer_legacy_or_new_uses_dot_berimor_for_fresh_workspace() {
        let dir = std::env::temp_dir().join(format!("berimor-state-fresh-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        let path = prefer_legacy_or_new(&dir, "berimor.db", "berimor.db");

        assert_eq!(path, dir.join(".berimor").join("berimor.db"));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Директива «старые проекты не трогать»: уже существующий легаси-
    /// файл в корне побеждает — ни переноса, ни молчаливой потери уже
    /// накопленного состояния.
    #[test]
    fn prefer_legacy_or_new_keeps_existing_root_file() {
        let dir = std::env::temp_dir().join(format!("berimor-state-legacy-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("berimor.db"), "").unwrap();

        let path = prefer_legacy_or_new(&dir, "berimor.db", "berimor.db");

        assert_eq!(path, dir.join("berimor.db"), "легаси-файл не переносится");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// `.berimor-allow` (легаси) → `.berimor/allow` (новые проекты) —
    /// тот же принцип, отдельная от журнала/конфига функция.
    #[test]
    fn project_allow_new_workspace_uses_dot_berimor_subdir() {
        let dir = std::env::temp_dir().join(format!("berimor-allow-fresh-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        append_project_allow(&dir, "files.write").unwrap();

        assert!(dir.join(".berimor").join("allow").is_file());
        assert!(!dir.join(".berimor-allow").exists());
        assert_eq!(load_project_allow(&dir), vec!["files.write".to_string()]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn project_allow_keeps_existing_legacy_file() {
        let dir = std::env::temp_dir().join(format!("berimor-allow-legacy-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(".berimor-allow"), "terminal.exec\n").unwrap();

        append_project_allow(&dir, "files.write").unwrap();

        let legacy = std::fs::read_to_string(dir.join(".berimor-allow")).unwrap();
        assert!(legacy.contains("terminal.exec"));
        assert!(
            legacy.contains("files.write"),
            "дописано в легаси-файл: {legacy}"
        );
        assert!(
            !dir.join(".berimor").join("allow").exists(),
            "новый файл не создан, пока легаси уже существует"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    // --- Контракты из конфигурации (спека config-contracts, 2026-08-14) ---

    /// Временная директория с файлом конфигурации (как в тестах выше —
    /// без гонок по глобальному CWD, путь задаётся явно).
    fn config_dir(tag: &str, contents: &str) -> (PathBuf, PathBuf) {
        let dir =
            std::env::temp_dir().join(format!("berimor-contracts-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(&path, contents).unwrap();
        (dir, path)
    }

    /// Inline-схема (спека п.5): объявление парсится и проходит
    /// валидацию при загрузке.
    #[test]
    fn contract_with_inline_schema_loads() {
        let (dir, path) = config_dir(
            "inline",
            r#"
[[contracts]]
name = "MeetingMinutes"
description = "протокол встречи"
schema = """{"type":"object","properties":{"summary":{"type":"string"}},"required":["summary"]}"""
"#,
        );

        let config = load(Some(&path)).unwrap();

        assert_eq!(config.contracts.len(), 1);
        let contract = &config.contracts[0];
        assert_eq!(contract.name, "MeetingMinutes");
        assert_eq!(contract.description.as_deref(), Some("протокол встречи"));
        assert!(contract.schema.is_some(), "inline-схема на месте");
        assert!(contract.schema_path.is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Схема файлом (спека п.5): `schema_path` читается относительно
    /// каталога файла конфигурации и разрешается в inline.
    #[test]
    fn contract_with_schema_path_resolves_relative_to_config_file() {
        let (dir, path) = config_dir(
            "path",
            r#"
[[contracts]]
name = "MeetingMinutes"
schema_path = "contracts/minutes.schema.json"
"#,
        );
        std::fs::create_dir_all(dir.join("contracts")).unwrap();
        std::fs::write(
            dir.join("contracts").join("minutes.schema.json"),
            r#"{"type":"object","properties":{"summary":{"type":"string"}},"required":["summary"]}"#,
        )
        .unwrap();

        let config = load(Some(&path)).unwrap();

        let contract = &config.contracts[0];
        assert!(
            contract.schema_path.is_none(),
            "путь разрешён в inline при загрузке"
        );
        let schema = contract.schema.as_deref().unwrap();
        assert!(schema.contains("\"summary\""), "содержимое файла: {schema}");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Дубликат имени кодового контракта (спека п.5) — ошибка загрузки:
    /// иначе поведение `berimor run` зависело бы от порядка поиска.
    #[test]
    fn contract_named_like_code_contract_is_rejected() {
        let (dir, path) = config_dir(
            "collision",
            r#"
[[contracts]]
name = "ClassificationOut"
schema = """{"type":"object"}"""
"#,
        );

        let result = load(Some(&path));

        match result {
            Err(ConfigError::Contract { contract, reason }) => {
                assert_eq!(contract, "ClassificationOut");
                assert!(reason.contains("кодовым контрактом"), "{reason}");
            }
            other => panic!("ожидалась ошибка ConfigError::Contract: {other:?}"),
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Дубликат имени внутри одного файла — ошибка загрузки.
    #[test]
    fn duplicate_contract_name_is_rejected() {
        let (dir, path) = config_dir(
            "dup",
            r#"
[[contracts]]
name = "MeetingMinutes"
schema = """{"type":"object"}"""

[[contracts]]
name = "MeetingMinutes"
schema = """{"type":"object"}"""
"#,
        );

        let result = load(Some(&path));

        assert!(
            matches!(result, Err(ConfigError::Contract { .. })),
            "дубликат имени обязан отклоняться: {result:?}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Битая схема (спека п.5: «компилируется jsonschema::validator_for»)
    /// — ошибка загрузки с текстом, не отложенный сбой шага.
    #[test]
    fn invalid_schema_is_rejected_at_load() {
        let (dir, path) = config_dir(
            "broken",
            r#"
[[contracts]]
name = "MeetingMinutes"
schema = """{"type":"no-such-json-type"}"""
"#,
        );

        let result = load(Some(&path));

        match result {
            Err(ConfigError::Contract { contract, reason }) => {
                assert_eq!(contract, "MeetingMinutes");
                assert!(
                    reason.contains("JSON Schema") || reason.contains("JSON"),
                    "понятный текст ошибки: {reason}"
                );
            }
            other => panic!("ожидалась ошибка ConfigError::Contract: {other:?}"),
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Схема, не парсящаяся как JSON, — та же ошибка загрузки.
    #[test]
    fn non_json_schema_is_rejected_at_load() {
        let (dir, path) = config_dir(
            "notjson",
            r#"
[[contracts]]
name = "MeetingMinutes"
schema = """{type: object}"""
"#,
        );

        let result = load(Some(&path));

        assert!(
            matches!(result, Err(ConfigError::Contract { .. })),
            "не-JSON схема обязана отклоняться: {result:?}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Обе формы схемы сразу — ошибка, не молчаливый приоритет одной.
    #[test]
    fn contract_with_both_schema_forms_is_rejected() {
        let (dir, path) = config_dir(
            "both",
            r#"
[[contracts]]
name = "MeetingMinutes"
schema = """{"type":"object"}"""
schema_path = "contracts/minutes.schema.json"
"#,
        );
        std::fs::create_dir_all(dir.join("contracts")).unwrap();
        std::fs::write(
            dir.join("contracts").join("minutes.schema.json"),
            r#"{"type":"object"}"#,
        )
        .unwrap();

        let result = load(Some(&path));

        assert!(
            matches!(result, Err(ConfigError::Contract { .. })),
            "две формы схемы обязаны отклоняться: {result:?}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Несуществующий schema_path — ошибка чтения с именем контракта.
    #[test]
    fn missing_schema_path_file_is_a_read_error() {
        let (dir, path) = config_dir(
            "missing",
            r#"
[[contracts]]
name = "MeetingMinutes"
schema_path = "contracts/absent.schema.json"
"#,
        );

        let result = load(Some(&path));

        match result {
            Err(ConfigError::ContractSchemaRead { contract, .. }) => {
                assert_eq!(contract, "MeetingMinutes")
            }
            other => panic!("ожидалась ошибка ContractSchemaRead: {other:?}"),
        }
        std::fs::remove_dir_all(&dir).ok();
    }
}
