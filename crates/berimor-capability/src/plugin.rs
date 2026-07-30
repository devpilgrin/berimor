//! ACL-манифест плагина: статическая декларация допустимых событий,
//! секретов и потолка capability — плагин не может переопределить это сам.
//!
//! Источник: `docs/arch/security-model.md` §4, ADR-0014. ROADMAP: S6.
//!
//! «Источник ACL — статический манифест на диске, который сам компонент
//! переопределить не может» (§4) — компонент здесь не только «плагин» в
//! узком смысле внешнего коннектора: та же схема — общая точка для
//! будущей проверки ACL топика акторов на шине событий (A2, пока
//! заблокирована — сама эта задача её и разблокирует), не отдельный
//! параллельный механизм. Тот же принцип, что ADR-0013 применил к памяти
//! актора: «одна ось изоляции/ACL, не две».
//!
//! Границы (честно, для ревью): «сам плагин не может переопределить
//! манифест» — это гарантия ПРОЦЕССНОЙ изоляции (плагин — отдельный
//! процесс с межпроцессным RPC, `ideal-agent-architecture.md` §3.9), не
//! то, что может обеспечить этот модуль в одиночку. Здесь — детерминированная
//! проверка предложенного действия против уже загруженного манифеста
//! («статическое применение» из названия задачи); то, что файл манифеста
//! на диске недоступен на запись процессу плагина — вопрос прав доступа
//! файловой системы при развёртывании, вне scope этого кода (тот же приём,
//! что TOCTOU у `jail.rs`, S2: задокументированная, не выдуманная граница).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Манифест — статический файл, загружаемый ХОСТОМ (не плагином) из
/// доверенного расположения. Пустые списки — намеренное значение по
/// умолчанию (`#[serde(default)]`): манифест без явно перечисленных
/// разрешений не разрешает НИЧЕГО — fail-closed, не «плагин может всё,
/// пока не запрещено».
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PluginManifest {
    pub name: String,
    /// Топики шины событий, в которые плагину разрешено публиковать
    /// (§4: «компонент не может публиковать события под чужим именем» —
    /// тот же принцип: не под чужим ИМЕНЕМ топика, которого нет в списке).
    #[serde(default)]
    pub allowed_events: Vec<String>,
    /// Имена секретов (`berimor_secrets`), к которым у плагина есть доступ.
    #[serde(default)]
    pub allowed_secrets: Vec<String>,
    /// Верхняя граница набора capability/инструментов, доступных
    /// плагину — имена инструментов или классов действий; сам плагин не
    /// может запросить больше, даже если инструмент технически ему доступен.
    #[serde(default)]
    pub capability_ceiling: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum PluginAclError {
    #[error("плагин '{plugin}' не имеет права публиковать в топик '{event}'")]
    EventNotAllowed { plugin: String, event: String },
    #[error("плагин '{plugin}' не имеет доступа к секрету '{secret}'")]
    SecretNotAllowed { plugin: String, secret: String },
    #[error("плагин '{plugin}' не имеет права на возможность '{capability}' (потолок манифеста)")]
    CapabilityNotAllowed { plugin: String, capability: String },
}

impl PluginManifest {
    pub fn check_event(&self, event: &str) -> Result<(), PluginAclError> {
        if self.allowed_events.iter().any(|e| e == event) {
            Ok(())
        } else {
            Err(PluginAclError::EventNotAllowed {
                plugin: self.name.clone(),
                event: event.to_string(),
            })
        }
    }

    pub fn check_secret(&self, secret: &str) -> Result<(), PluginAclError> {
        if self.allowed_secrets.iter().any(|s| s == secret) {
            Ok(())
        } else {
            Err(PluginAclError::SecretNotAllowed {
                plugin: self.name.clone(),
                secret: secret.to_string(),
            })
        }
    }

    pub fn check_capability(&self, capability: &str) -> Result<(), PluginAclError> {
        if self.capability_ceiling.iter().any(|c| c == capability) {
            Ok(())
        } else {
            Err(PluginAclError::CapabilityNotAllowed {
                plugin: self.name.clone(),
                capability: capability.to_string(),
            })
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    #[error("не удалось прочитать манифест плагина {path}: {reason}")]
    Read { path: String, reason: String },
    #[error("не удалось разобрать манифест плагина {path}: {reason}")]
    Parse { path: String, reason: String },
    /// Два файла манифеста объявляют одно и то же `name` — какой из них
    /// реален, неоднозначно; молчаливая перезапись одного другим в
    /// реестре была бы тихой дырой в ACL (не отказ, а подмена).
    #[error("манифест плагина с именем '{0}' уже загружен из другого файла")]
    DuplicateName(String),
}

pub fn load_manifest(path: &Path) -> Result<PluginManifest, ManifestError> {
    let text = std::fs::read_to_string(path).map_err(|err| ManifestError::Read {
        path: path.display().to_string(),
        reason: err.to_string(),
    })?;
    serde_norway::from_str(&text).map_err(|err| ManifestError::Parse {
        path: path.display().to_string(),
        reason: err.to_string(),
    })
}

/// Реестр манифестов — загружается один раз хостом (обычно при старте) из
/// доверенного каталога, отдаёт read-only ссылки по имени плагина. У
/// самого плагина нет доступа к этому типу — только к своему процессу.
pub struct PluginRegistry {
    manifests: HashMap<String, PluginManifest>,
}

impl PluginRegistry {
    /// Загружает все `*.yaml`/`*.yml`-файлы из `dir` как манифесты — имя
    /// файла не участвует в идентификации плагина, только поле `name`
    /// внутри (иначе переименование файла на диске тихо меняло бы
    /// идентичность плагина без изменения его прав).
    pub fn load_dir(dir: &Path) -> Result<Self, ManifestError> {
        let mut manifests = HashMap::new();
        let entries = std::fs::read_dir(dir).map_err(|err| ManifestError::Read {
            path: dir.display().to_string(),
            reason: err.to_string(),
        })?;
        for entry in entries {
            let entry = entry.map_err(|err| ManifestError::Read {
                path: dir.display().to_string(),
                reason: err.to_string(),
            })?;
            let path: PathBuf = entry.path();
            let is_yaml = path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.eq_ignore_ascii_case("yaml") || e.eq_ignore_ascii_case("yml"))
                .unwrap_or(false);
            if !is_yaml {
                continue;
            }
            let manifest = load_manifest(&path)?;
            if manifests.contains_key(&manifest.name) {
                return Err(ManifestError::DuplicateName(manifest.name));
            }
            manifests.insert(manifest.name.clone(), manifest);
        }
        Ok(Self { manifests })
    }

    pub fn get(&self, plugin_name: &str) -> Option<&PluginManifest> {
        self.manifests.get(plugin_name)
    }

    pub fn len(&self) -> usize {
        self.manifests.len()
    }

    pub fn is_empty(&self) -> bool {
        self.manifests.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest() -> PluginManifest {
        PluginManifest {
            name: "crm-connector".into(),
            allowed_events: vec!["crm.card_status_requested".into()],
            allowed_secrets: vec!["crm_api_key".into()],
            capability_ceiling: vec!["net.http".into()],
        }
    }

    #[test]
    fn check_event_allows_declared_topic() {
        assert!(manifest().check_event("crm.card_status_requested").is_ok());
    }

    #[test]
    fn check_event_rejects_undeclared_topic() {
        let result = manifest().check_event("crm.card_deleted");
        assert_eq!(
            result,
            Err(PluginAclError::EventNotAllowed {
                plugin: "crm-connector".into(),
                event: "crm.card_deleted".into(),
            })
        );
    }

    #[test]
    fn check_secret_allows_declared_secret() {
        assert!(manifest().check_secret("crm_api_key").is_ok());
    }

    #[test]
    fn check_secret_rejects_undeclared_secret() {
        assert!(manifest().check_secret("другой_секрет").is_err());
    }

    #[test]
    fn check_capability_allows_declared_capability() {
        assert!(manifest().check_capability("net.http").is_ok());
    }

    #[test]
    fn check_capability_rejects_undeclared_capability() {
        assert!(manifest().check_capability("fs.write").is_err());
    }

    #[test]
    fn empty_manifest_lists_deny_everything_fail_closed() {
        let empty = PluginManifest {
            name: "bare".into(),
            allowed_events: vec![],
            allowed_secrets: vec![],
            capability_ceiling: vec![],
        };
        assert!(empty.check_event("anything").is_err());
        assert!(empty.check_secret("anything").is_err());
        assert!(empty.check_capability("anything").is_err());
    }

    fn write_manifest(dir: &Path, filename: &str, yaml: &str) {
        std::fs::write(dir.join(filename), yaml).unwrap();
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("berimor-plugin-test-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn load_manifest_parses_a_well_formed_file() {
        let dir = temp_dir("load-one");
        write_manifest(
            &dir,
            "crm.yaml",
            "name: crm-connector\nallowed_events: [\"crm.card_status_requested\"]\nallowed_secrets: [\"crm_api_key\"]\ncapability_ceiling: [\"net.http\"]\n",
        );

        let loaded = load_manifest(&dir.join("crm.yaml")).unwrap();
        assert_eq!(loaded, manifest());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn load_manifest_missing_file_is_a_read_error() {
        let result = load_manifest(Path::new("/nonexistent/path/manifest.yaml"));
        assert!(matches!(result, Err(ManifestError::Read { .. })));
    }

    #[test]
    fn load_manifest_malformed_yaml_is_a_parse_error_not_a_guess() {
        let dir = temp_dir("malformed");
        write_manifest(&dir, "bad.yaml", "это: не: валидный: манифест: - - -");

        let result = load_manifest(&dir.join("bad.yaml"));
        assert!(matches!(result, Err(ManifestError::Parse { .. })));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn load_manifest_missing_optional_fields_defaults_to_empty_lists() {
        let dir = temp_dir("minimal");
        write_manifest(&dir, "minimal.yaml", "name: bare\n");

        let loaded = load_manifest(&dir.join("minimal.yaml")).unwrap();
        assert!(loaded.allowed_events.is_empty());
        assert!(loaded.allowed_secrets.is_empty());
        assert!(loaded.capability_ceiling.is_empty());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn registry_loads_all_yaml_files_and_looks_up_by_manifest_name_not_filename() {
        let dir = temp_dir("registry");
        write_manifest(&dir, "arbitrary-filename.yaml", "name: crm-connector\n");
        write_manifest(&dir, "other.yml", "name: billing-connector\n");
        write_manifest(&dir, "ignored.txt", "not a manifest at all");

        let registry = PluginRegistry::load_dir(&dir).unwrap();

        assert_eq!(registry.len(), 2);
        assert!(registry.get("crm-connector").is_some());
        assert!(registry.get("billing-connector").is_some());
        assert!(registry.get("arbitrary-filename").is_none());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn registry_rejects_duplicate_manifest_names_across_files() {
        let dir = temp_dir("duplicate");
        write_manifest(&dir, "a.yaml", "name: same-name\n");
        write_manifest(&dir, "b.yaml", "name: same-name\n");

        let result = PluginRegistry::load_dir(&dir);

        assert!(matches!(result, Err(ManifestError::DuplicateName(name)) if name == "same-name"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn registry_of_empty_directory_is_empty_not_an_error() {
        let dir = temp_dir("empty");
        let registry = PluginRegistry::load_dir(&dir).unwrap();
        assert!(registry.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn registry_get_unknown_plugin_is_none() {
        let dir = temp_dir("lookup-miss");
        let registry = PluginRegistry::load_dir(&dir).unwrap();
        assert!(registry.get("no-such-plugin").is_none());
        std::fs::remove_dir_all(&dir).ok();
    }
}
