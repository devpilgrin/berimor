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
            entity_graph: false,
            fact_extraction: false,
            embeddings: false,
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
    /// выше — разрешение снимает ВОПРОС, не запрет. Плюс файл
    /// `.berimor-allow` в корне области (пишет модал «для проекта»).
    #[serde(default)]
    pub auto_confirm: Vec<String>,
    #[serde(default)]
    pub serve: ServeConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            storage_path: PathBuf::from("./berimor.db"),
            confirmation_mode: ConfirmationMode::Smart,
            update_channel: UpdateChannel::Stable,
            providers: Vec::new(),
            tool_stubs: Vec::new(),
            memory: MemoryConfig::default(),
            mcp_servers: Vec::new(),
            secret_envs: Vec::new(),
            auto_confirm: Vec::new(),
            serve: ServeConfig::default(),
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
}

// --- Слоистая конфигурация (§20.12) ------------------------------------

/// Путь локальной конфигурации по умолчанию — `./berimor.toml` в
/// текущей директории (слой проекта поверх глобального, §20.12).
pub fn default_config_path() -> PathBuf {
    PathBuf::from("./berimor.toml")
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
    pub memory: Option<MemoryConfig>,
    pub mcp_servers: Vec<McpServerConfig>,
    pub secret_envs: Vec<String>,
    #[serde(default)]
    pub auto_confirm: Vec<String>,
    pub serve: Option<ServeConfig>,
}

impl PartialConfig {
    fn load_file(path: &Path) -> Result<Option<Self>, ConfigError> {
        match std::fs::read_to_string(path) {
            Ok(contents) => {
                toml::from_str(&contents)
                    .map(Some)
                    .map_err(|source| ConfigError::Parse {
                        path: path.to_path_buf(),
                        source,
                    })
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(ConfigError::Read {
                path: path.to_path_buf(),
                source,
            }),
        }
    }
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
            .unwrap_or(defaults.storage_path),
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
        memory: local.memory.or(global.memory).unwrap_or(defaults.memory),
        mcp_servers: merge_named(global.mcp_servers, local.mcp_servers, |s| s.name.clone()),
        secret_envs,
        auto_confirm,
        serve: local.serve.or(global.serve).unwrap_or(defaults.serve),
    }
}

/// Проектные разрешения на мутации: `.berimor-allow` в корне области —
/// по одному имени инструмента на строку (`#` — комментарии). Пишет
/// модал подтверждения («разрешить для проекта»), читается при сборке
/// бандла. Файл, а не ключ в TOML: дописывать строку честнее, чем
/// переписывать пользовательский конфиг сериализатором.
pub fn load_project_allow(workspace: &std::path::Path) -> Vec<String> {
    let Ok(contents) = std::fs::read_to_string(workspace.join(".berimor-allow")) else {
        return Vec::new();
    };
    contents
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(str::to_string)
        .collect()
}

/// Дописывает разрешение в `.berimor-allow` области (идемпотентно).
pub fn append_project_allow(workspace: &std::path::Path, tool: &str) -> std::io::Result<()> {
    if load_project_allow(workspace).iter().any(|t| t == tool) {
        return Ok(());
    }
    use std::io::Write as _;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(workspace.join(".berimor-allow"))?;
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
    Ok(merge(global.unwrap_or_default(), local.unwrap_or_default()))
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(
            config.storage_path,
            Config::default().storage_path,
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
}
