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

use berimor_types::capability::ConfirmationMode;
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

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Путь к файлу журнала SQLite (`berimor-storage::SqliteEventLog`).
    pub storage_path: PathBuf,
    /// `smart` — интерактивный режим по умолчанию (security-model.md §3).
    pub confirmation_mode: ConfirmationMode,
    pub update_channel: UpdateChannel,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            storage_path: PathBuf::from("./berimor.db"),
            confirmation_mode: ConfirmationMode::Smart,
            update_channel: UpdateChannel::Stable,
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
