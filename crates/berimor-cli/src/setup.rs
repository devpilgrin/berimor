//! Мастер первичной настройки (§20.12): `berimor setup` и автоматический
//! запуск при первом старте без конфигурации. Пишет ГЛОБАЛЬНЫЙ конфиг
//! (`~/.config/berimor/config.toml`) и ключи — в `secrets.env` с правами
//! 0600 рядом (security-model.md §6: секретов в конфиге нет, есть имена
//! переменных; env-файл подхватывается при загрузке, явное окружение
//! сильнее файла).

use crate::config::{self, ProviderConfig};
use crate::presets::{self, ProviderPreset};
use std::io::Write;
use std::path::Path;

/// Ошибки мастера — свой тип, не RunError: setup вызывается и до
/// сборки рантайма (first-run), и из чата (перезагрузка).
#[derive(Debug, thiserror::Error)]
pub enum SetupError {
    #[error("не удалось определить глобальную директорию (нет HOME/XDG_CONFIG_HOME)")]
    NoGlobalDir,
    #[error("ввод-вывод: {0}")]
    Io(#[from] std::io::Error),
    #[error("конфигурация: {0}")]
    Config(#[from] config::ConfigError),
}

/// Дописывает провайдеров в глобальный конфиг, не затирая существующие
/// записи и чужие правки: файл парсится, провайдеры с уже занятыми
/// именами пропускаются (повторный запуск мастера безопасен). Возвращает
/// имена реально добавленных.
pub fn append_providers(
    global_path: &Path,
    providers: &[ProviderConfig],
) -> Result<Vec<String>, SetupError> {
    let existing: config::PartialConfig = match std::fs::read_to_string(global_path) {
        Ok(text) => toml::from_str(&text).map_err(|source| config::ConfigError::Parse {
            path: global_path.to_path_buf(),
            source,
        })?,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Default::default(),
        Err(source) => {
            return Err(config::ConfigError::Read {
                path: global_path.to_path_buf(),
                source,
            }
            .into())
        }
    };
    let taken: std::collections::HashSet<&str> =
        existing.providers.iter().map(|p| p.name.as_str()).collect();
    let mut added = Vec::new();
    let mut block = String::new();
    for provider in providers {
        if taken.contains(provider.name.as_str()) {
            continue;
        }
        block.push_str(&presets::render_provider_toml(provider));
        added.push(provider.name.clone());
    }
    if block.is_empty() {
        return Ok(added);
    }
    if let Some(parent) = global_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(global_path)?;
    file.write_all(block.as_bytes())?;
    Ok(added)
}

/// Дописывает ключ в `secrets.env` (0600, владелец-only). Имя уже
/// присутствует — не дублирует (значение не перезаписывается молча:
/// смена ключа — осознанное редактирование файла или удаление строки).
pub fn append_secret(secrets_path: &Path, env_name: &str, value: &str) -> Result<bool, SetupError> {
    if let Ok(existing) = std::fs::read_to_string(secrets_path) {
        if config::parse_secrets_env(&existing)
            .iter()
            .any(|(name, _)| name == env_name)
        {
            return Ok(false);
        }
    }
    if let Some(parent) = secrets_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(secrets_path)?;
    file.write_all(format!("{env_name}={value}\n").as_bytes())?;
    // 0600: файл с ключами читается только владельцем — тот же уровень,
    // что ~/.ssh/id_*.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(secrets_path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(true)
}

fn read_line(prompt: &str) -> Result<String, SetupError> {
    eprint!("{prompt}");
    std::io::stderr().flush()?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    Ok(line.trim().to_string())
}

/// Интерактивная часть: меню пресетов → выбор номеров → правки
/// model_id/base_url → ключи. Возвращает (провайдеры, ключи) — запись
/// делает [`run_wizard`], чтобы логику выбора можно было гонять в
/// тестах через подмену ввода отдельно от файловой системы.
pub fn run_wizard() -> Result<Vec<String>, SetupError> {
    let global_path = config::global_config_path().ok_or(SetupError::NoGlobalDir)?;
    let secrets_path = config::secrets_env_path().ok_or(SetupError::NoGlobalDir)?;

    eprintln!(
        "[berimor] мастер настройки — глобальный конфиг: {}",
        global_path.display()
    );
    eprintln!("[berimor] доступные пресеты:");
    for (i, preset) in presets::PRESETS.iter().enumerate() {
        eprintln!(
            "  {}. {} — {} (модель по умолчанию: {})",
            i + 1,
            preset.display,
            preset.about,
            preset.default_model
        );
    }
    let selection =
        read_line("Номера или имена через запятую (2,ollama), Enter — пропустить мастер: ")?;
    if selection.is_empty() {
        return Ok(Vec::new());
    }

    let mut chosen: Vec<ProviderConfig> = Vec::new();
    let mut keys: Vec<(String, String)> = Vec::new();
    for token in selection
        .split(',')
        .map(str::trim)
        .filter(|t| !t.is_empty())
    {
        // Номер из меню ИЛИ имя пресета («2,ollama» и «deepseek» —
        // равноправны: имя устойчиво к перестановкам меню в скриптах).
        let preset = token
            .parse::<usize>()
            .ok()
            .and_then(|index| index.checked_sub(1))
            .and_then(|zero| presets::PRESETS.get(zero))
            .or_else(|| presets::find_preset(token));
        let Some(preset) = preset else {
            eprintln!("[berimor] пропускаю «{token}» — нет такого номера или имени");
            continue;
        };
        let provider = configure_preset(preset)?;
        if let Some(env_name) = preset.key_env {
            if std::env::var_os(env_name).is_some() {
                eprintln!("[berimor] {env_name} уже задан в окружении — файл не нужен");
            } else {
                let key = read_line(&format!(
                    "Ключ API для {} (Enter — задам позже в {env_name}): ",
                    preset.display
                ))?;
                if !key.is_empty() {
                    keys.push((env_name.to_string(), key));
                }
            }
        }
        chosen.push(provider);
    }

    let added = append_providers(&global_path, &chosen)?;
    let mut keys_written = 0;
    for (env_name, value) in &keys {
        if append_secret(&secrets_path, env_name, value)? {
            keys_written += 1;
        }
    }
    if !added.is_empty() {
        eprintln!("[berimor] добавлены провайдеры: {}", added.join(", "));
    } else {
        eprintln!("[berimor] новых провайдеров нет (имена уже заняты или ничего не выбрано)");
    }
    if keys_written > 0 {
        eprintln!(
            "[berimor] ключи записаны в {} (права 0600)",
            secrets_path.display()
        );
    }
    eprintln!("[berimor] проверка: berimor config show");
    Ok(added)
}

fn configure_preset(preset: &ProviderPreset) -> Result<ProviderConfig, SetupError> {
    let model = read_line(&format!(
        "  {}: model_id [{}]: ",
        preset.display, preset.default_model
    ))?;
    let mut base_url = None;
    if preset.private {
        let url = read_line(&format!(
            "  {}: base_url [{}]: ",
            preset.display, preset.base_url
        ))?;
        if !url.is_empty() {
            base_url = Some(url);
        }
    }
    Ok(presets::instantiate(
        preset,
        if model.is_empty() { None } else { Some(model) },
        base_url,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_providers_is_idempotent_and_non_destructive() {
        let dir = std::env::temp_dir().join(format!("berimor-setup-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(&path, "confirmation_mode = \"manual\"\n").unwrap();

        let deepseek = presets::instantiate(presets::find_preset("deepseek").unwrap(), None, None);
        let added = append_providers(&path, std::slice::from_ref(&deepseek)).unwrap();
        assert_eq!(added, vec!["deepseek"]);
        // Повторный запуск — не дублирует.
        let added_again = append_providers(&path, &[deepseek]).unwrap();
        assert!(added_again.is_empty());

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("confirmation_mode = \"manual\""),
            "чужие правки целы"
        );
        let parsed: config::PartialConfig = toml::from_str(&text).unwrap();
        assert_eq!(parsed.providers.len(), 1);
        assert_eq!(
            parsed.confirmation_mode,
            Some(berimor_types::capability::ConfirmationMode::Manual)
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn append_secret_writes_owner_only_and_never_duplicates() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("berimor-secrets-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("secrets.env");

        assert!(append_secret(&path, "TEST_KEY_X", "v1").unwrap());
        // То же имя — не перезаписывается молча.
        assert!(!append_secret(&path, "TEST_KEY_X", "v2").unwrap());

        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text, "TEST_KEY_X=v1\n");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "владелец-only: {mode:o}");
        std::fs::remove_dir_all(&dir).ok();
    }
}
