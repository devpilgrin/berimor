//! `berimor trust {add,remove,list}` (ROADMAP D5) — доверенный список
//! репозиториев. Источник: `docs/arch/deployment.md` §4.
//!
//! «Изменение списка — событие, не сетевой эффект. Добавление или
//! удаление записи показывается человеку как diff и требует
//! подтверждения (I2) прежде чем применяется к состоянию» — `add`/
//! `remove` печатают предлагаемую запись и спрашивают подтверждение ДО
//! `storage.append`; отказ — ничего не пишется в журнал.
//!
//! Свёртка текущего состояния — `berimor_capability::trust_list::
//! fold_trust_list` над журналом под синтетическим `ProcessInstanceId`
//! (`trust_list::TRUST_LIST_INSTANCE_ID`) — см. doc-комментарий этого
//! модуля про то, почему не отдельная таблица.

use crate::config::Config;
use crate::run::ask_line;
use berimor_capability::trust_list::{
    fold_trust_list, is_plausible_signer_identity, TrustedRepoEntry, TRUST_LIST_INSTANCE_ID,
};
use berimor_storage::{EventLog, SqliteEventLog};
use berimor_types::event::{Event, EventKind, ProcessInstanceId, TrustListAction};
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum TrustError {
    #[error("не удалось открыть журнал {path}: {reason}")]
    OpenStorage { path: PathBuf, reason: String },
    #[error("не удалось прочитать журнал доверенного списка: {0}")]
    Replay(String),
    #[error("не удалось записать изменение доверенного списка: {0}")]
    Append(String),
    #[error("репозиторий '{0}' не найден в доверенном списке")]
    NotFound(String),
    #[error("изменение отклонено")]
    Declined,
    #[error("signer_workflow '{0}' не похож на URL workflow-файла (ожидается 'https://github.com/<owner>/<repo>/.github/workflows/<file>.yml@...')")]
    ImplausibleSignerIdentity(String),
}

fn open_storage(config: &Config) -> Result<SqliteEventLog, TrustError> {
    SqliteEventLog::open(&config.storage_path).map_err(|err| TrustError::OpenStorage {
        path: config.storage_path.clone(),
        reason: err.to_string(),
    })
}

fn trust_list_instance_id() -> ProcessInstanceId {
    ProcessInstanceId(TRUST_LIST_INSTANCE_ID.to_string())
}

fn current_list(storage: &dyn EventLog) -> Result<HashMap<String, TrustedRepoEntry>, TrustError> {
    let events = storage
        .replay(&trust_list_instance_id())
        .map_err(|err| TrustError::Replay(err.to_string()))?;
    Ok(fold_trust_list(&events))
}

pub fn add(
    config: &Config,
    repo: &str,
    allowed_ref: &str,
    signer_workflow: &str,
    capability_ceiling: &[String],
) -> Result<(), TrustError> {
    add_with_confirm(
        config,
        repo,
        allowed_ref,
        signer_workflow,
        capability_ceiling,
        || ask_line("[berimor] подтвердить добавление? [y/N] "),
    )
}

/// Отделено от [`add`] ради тестируемости — реальный `ask_line` читает
/// stdin процесса, тесты подставляют детерминированное `|| true`/`|| false`
/// (тот же DI-принцип, что `ConfirmationHandler` в `self_update.rs`/`run.rs`).
fn add_with_confirm(
    config: &Config,
    repo: &str,
    allowed_ref: &str,
    signer_workflow: &str,
    capability_ceiling: &[String],
    confirm: impl FnOnce() -> bool,
) -> Result<(), TrustError> {
    // Независимое ревью (MAJOR-3): пустой/не похожий на URL
    // signer_workflow делает `ReleaseWorkflowPath::verify` (`verify.rs`)
    // проверкой-заглушкой (`"...".starts_with("")` истинно для ЛЮБОГО
    // SAN) — здесь запись НАВСЕГДА уходит в журнал, ослабляя проверку
    // для всех будущих установок из этого репозитория, поэтому отказ ДО
    // показа diff/подтверждения, не после.
    if !is_plausible_signer_identity(signer_workflow) {
        return Err(TrustError::ImplausibleSignerIdentity(
            signer_workflow.to_string(),
        ));
    }
    let storage = open_storage(config)?;
    println!("[berimor] добавление в доверенный список:");
    println!("  repo:               {repo}");
    println!("  allowed_ref:        {allowed_ref}");
    println!("  signer_identity:    {signer_workflow}");
    println!("  capability_ceiling: {}", capability_ceiling.join(", "));
    if !confirm() {
        return Err(TrustError::Declined);
    }
    storage
        .append(Event::new(
            trust_list_instance_id(),
            0,
            EventKind::TrustListChanged {
                action: TrustListAction::Added,
                repo: repo.to_string(),
                allowed_ref: allowed_ref.to_string(),
                signer_identity: signer_workflow.to_string(),
                capability_ceiling: capability_ceiling.to_vec(),
            },
            Value::Null,
        ))
        .map_err(|err| TrustError::Append(err.to_string()))?;
    println!("[berimor] добавлено");
    Ok(())
}

pub fn remove(config: &Config, repo: &str) -> Result<(), TrustError> {
    remove_with_confirm(config, repo, || {
        ask_line("[berimor] подтвердить удаление? [y/N] ")
    })
}

fn remove_with_confirm(
    config: &Config,
    repo: &str,
    confirm: impl FnOnce() -> bool,
) -> Result<(), TrustError> {
    let storage = open_storage(config)?;
    let list = current_list(&storage)?;
    if !list.contains_key(repo) {
        return Err(TrustError::NotFound(repo.to_string()));
    }
    println!("[berimor] удаление из доверенного списка: {repo}");
    if !confirm() {
        return Err(TrustError::Declined);
    }
    storage
        .append(Event::new(
            trust_list_instance_id(),
            0,
            EventKind::TrustListChanged {
                action: TrustListAction::Removed,
                repo: repo.to_string(),
                allowed_ref: String::new(),
                signer_identity: String::new(),
                capability_ceiling: Vec::new(),
            },
            Value::Null,
        ))
        .map_err(|err| TrustError::Append(err.to_string()))?;
    println!("[berimor] удалено");
    Ok(())
}

pub fn list(config: &Config) -> Result<(), TrustError> {
    let storage = open_storage(config)?;
    let list = current_list(&storage)?;
    if list.is_empty() {
        println!("[berimor] доверенный список пуст");
        return Ok(());
    }
    let mut repos: Vec<&TrustedRepoEntry> = list.values().collect();
    repos.sort_by(|a, b| a.repo.cmp(&b.repo));
    for entry in repos {
        println!(
            "{}  allowed_ref={}  capability_ceiling=[{}]  signer_identity={}",
            entry.repo,
            entry.allowed_ref,
            entry.capability_ceiling.join(", "),
            entry.signer_identity,
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_config() -> Config {
        let path = std::env::temp_dir().join(format!(
            "berimor-trust-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        Config {
            storage_path: path,
            ..Config::default()
        }
    }

    #[test]
    fn add_confirmed_persists_and_shows_in_list() {
        let config = temp_config();
        add_with_confirm(
            &config,
            "owner/plugin",
            "v*.*.*",
            "https://github.com/owner/plugin/.github/workflows/release.yml@",
            &["net.http".to_string()],
            || true,
        )
        .unwrap();

        let storage = open_storage(&config).unwrap();
        let list = current_list(&storage).unwrap();
        assert!(list.contains_key("owner/plugin"));
        std::fs::remove_file(&config.storage_path).ok();
    }

    #[test]
    fn add_declined_persists_nothing() {
        let config = temp_config();
        let err = add_with_confirm(
            &config,
            "owner/plugin",
            "v*.*.*",
            "https://github.com/owner/plugin/.github/workflows/release.yml@",
            &["net.http".to_string()],
            || false,
        )
        .unwrap_err();
        assert!(matches!(err, TrustError::Declined));

        let storage = open_storage(&config).unwrap();
        let list = current_list(&storage).unwrap();
        assert!(list.is_empty());
        std::fs::remove_file(&config.storage_path).ok();
    }

    /// Независимое ревью (MAJOR-3): пустой `signer_workflow` обязан
    /// отклоняться ДО показа diff/подтверждения — не только не
    /// записываться при отказе, но и не доходить до вопроса вовсе.
    #[test]
    fn add_with_empty_signer_workflow_is_rejected_before_asking_confirmation() {
        let config = temp_config();
        let err = add_with_confirm(&config, "owner/plugin", "v*.*.*", "", &[], || {
            panic!("не должно спрашивать подтверждение для неправдоподобного signer_workflow")
        })
        .unwrap_err();
        assert!(matches!(err, TrustError::ImplausibleSignerIdentity(_)));

        let storage = open_storage(&config).unwrap();
        assert!(current_list(&storage).unwrap().is_empty());
        std::fs::remove_file(&config.storage_path).ok();
    }

    #[test]
    fn remove_confirmed_removes_an_existing_entry() {
        let config = temp_config();
        add_with_confirm(
            &config,
            "owner/plugin",
            "v*.*.*",
            "https://github.com/owner/plugin/.github/workflows/release.yml@",
            &[],
            || true,
        )
        .unwrap();
        remove_with_confirm(&config, "owner/plugin", || true).unwrap();

        let storage = open_storage(&config).unwrap();
        assert!(current_list(&storage).unwrap().is_empty());
        std::fs::remove_file(&config.storage_path).ok();
    }

    #[test]
    fn remove_declined_keeps_the_entry() {
        let config = temp_config();
        add_with_confirm(
            &config,
            "owner/plugin",
            "v*.*.*",
            "https://github.com/owner/plugin/.github/workflows/release.yml@",
            &[],
            || true,
        )
        .unwrap();
        let err = remove_with_confirm(&config, "owner/plugin", || false).unwrap_err();
        assert!(matches!(err, TrustError::Declined));

        let storage = open_storage(&config).unwrap();
        assert!(current_list(&storage).unwrap().contains_key("owner/plugin"));
        std::fs::remove_file(&config.storage_path).ok();
    }

    #[test]
    fn remove_unknown_repo_is_not_found_not_a_confirmation_prompt() {
        let config = temp_config();
        let err = remove_with_confirm(&config, "owner/never-added", || {
            panic!("не должно спрашивать подтверждение для несуществующей записи")
        })
        .unwrap_err();
        assert!(matches!(err, TrustError::NotFound(_)));
        std::fs::remove_file(&config.storage_path).ok();
    }

    #[test]
    fn list_on_empty_storage_does_not_error() {
        let config = temp_config();
        list(&config).unwrap();
        std::fs::remove_file(&config.storage_path).ok();
    }
}
