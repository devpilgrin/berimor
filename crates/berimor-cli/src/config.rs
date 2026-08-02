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
    /// хранилища секретов»).
    pub api_key_env: Option<String>,
    /// Явный opt-in на приватный endpoint (сетевой гейт S3) — для
    /// локальных серверов инференса и тестовых моков.
    #[serde(default)]
    pub allow_private_endpoint: bool,
    /// Стоимость из прайс-таблицы владельца (код-данные, ADR-0011).
    #[serde(default)]
    pub cost_per_1k_tokens: Option<f64>,
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
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            skills_dir: None,
            session_search_limit: 5,
            entity_graph: false,
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

/// Путь конфигурации по умолчанию — `./berimor.toml` в текущей директории.
/// Осознанное упрощение для скелета: платформенные каталоги конфигурации
/// (XDG и т.п.) добавляются отдельной задачей, когда появится реальный
/// сценарий использования, диктующий их выбор — не раньше.
pub fn default_config_path() -> PathBuf {
    PathBuf::from("./berimor.toml")
}

/// Загружает конфигурацию: явный путь, если указан, иначе
/// [`default_config_path`]. Отсутствие файла по пути по умолчанию — не
/// ошибка, используются значения [`Config::default`]; отсутствие файла по
/// явно указанному пути — тоже не ошибка (явный путь может быть заготовкой
/// для будущего файла), но нечитаемый или невалидный существующий файл —
/// ошибка: молчаливо игнорировать испорченную конфигурацию нельзя (I2).
pub fn load(explicit_path: Option<&Path>) -> Result<Config, ConfigError> {
    let path = explicit_path
        .map(Path::to_path_buf)
        .unwrap_or_else(default_config_path);

    let contents = match std::fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Config::default());
        }
        Err(source) => return Err(ConfigError::Read { path, source }),
    };

    toml::from_str(&contents).map_err(|source| ConfigError::Parse { path, source })
}

#[cfg(test)]
mod tests {
    use super::*;

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
    }

    #[test]
    fn missing_file_at_default_path_falls_back_to_defaults() {
        let config = load(Some(Path::new("/nonexistent/path/does-not-exist.toml"))).unwrap();
        assert_eq!(config.storage_path, Config::default().storage_path);
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
