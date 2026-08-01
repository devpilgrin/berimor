//! `berimor plugin install <repo>` (ROADMAP D6) — установка плагина из
//! доверенного репозитория, процессом Process Engine — та же архитектура,
//! что D4 (`self_update.rs`): выделенный `ToolDispatch`/`StepExecutor`,
//! изолированный и от `run.rs::build_executor_bundle`, и от
//! `SelfUpdateDispatch` — три непересекающихся периметра инструментов на
//! весь CLI.
//!
//! Источник: `docs/arch/deployment.md` §6. Тот же граф — тот же принцип,
//! что D4: реальный `NextStep::Finished` наступает только когда для шага
//! нет следующего элемента в `steps` (`graph.rs::next_step`), «done» не
//! спецсентинел — граф процесса (`fixtures/golden/processes/
//! plugin-install.yaml`) физически спроектирован так же, как
//! `agent-self-update.yaml`.
//!
//! **Циркулярность первого доверия новому репозиторию.** У доверенного
//! списка (D5) нет способа узнать `signer_identity` НОВОГО репозитория
//! заранее — так же, как SHA-256-пин в D3 не мог быть вычислен изнутри
//! самого скачиваемого артефакта. Решение — TOFU (trust-on-first-use, тот
//! же принцип, что при первом SSH-подключении): оператор явно указывает
//! ожидаемую идентичность через `--signer-workflow`/`--capability-ceiling`/
//! `--allowed-ref` при установке из НОВОГО репозитория; для уже
//! доверенного — эти флаги игнорируются, используется существующая
//! запись (`deployment.md`: «запрос на расширение capability_ceiling для
//! уже доверенного репозитория — отдельное подтверждение, не входит в
//! исходное доверие репозиторию»).
//!
//! **Формат плагин-релиза** (проектное решение — источник не даёт
//! рецепта): один архив `<repo-basename>-<version>-<platform>-<arch>.
//! tar.gz` на GitHub Release репозитория плагина, содержащий И
//! исполняемый файл, И `manifest.yaml` (формат — уже существующий
//! `berimor_capability::plugin::PluginManifest`) — одна подпись на оба,
//! не два отдельных бандла.

use berimor_capability::confirm::{StandardCapability, ToolPolicy};
use berimor_capability::net_gate::{self, NetworkDecision};
use berimor_capability::plugin::load_manifest;
use berimor_capability::trust_list::{
    fold_trust_list, is_plausible_signer_identity, TRUST_LIST_INSTANCE_ID,
};
use berimor_capability::CapabilityGate;
use berimor_executors::tool_only::{self, ConfirmationHandler, DispatchError, ToolDispatch};
use berimor_process_engine::{
    engine::{self, ExecutorError},
    parser,
};
use berimor_storage::{EventLog, SqliteEventLog};
use berimor_types::capability::ConfirmationMode;
use berimor_types::event::{Event, EventKind, ProcessInstanceId, TrustListAction};
use berimor_types::step::{Patch, Step, StepKind};
use reqwest::blocking::Client;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use crate::config::Config;
use crate::run::{ask_human, interpolate, TerminalConfirmer};

const PROCESS_YAML: &str = include_str!("../../../fixtures/golden/processes/plugin-install.yaml");
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, thiserror::Error)]
pub enum PluginInstallRunError {
    #[error("не удалось разобрать встроенный процесс plugin-install: {0}")]
    ParseProcess(String),
    #[error("не удалось открыть журнал {path}: {reason}")]
    OpenStorage { path: PathBuf, reason: String },
    #[error("движок: {0}")]
    Engine(#[from] engine::EngineError),
    #[error("не удалось собрать plugin-install диспетчер: {0}")]
    Dispatch(String),
    #[error("выполнение остановлено на шаге human_gate: человек отклонил продолжение")]
    HumanDeclined,
    #[error(
        "'{0}' — зарезервированный идентификатор журнала доверенного списка, не process instance"
    )]
    ReservedInstanceId(String),
}

/// Каталог установленных плагинов и их манифестов — платформенный
/// каталог данных (`~/.local/share` на Linux, `~/Library/Application
/// Support` на macOS, `%APPDATA%` на Windows) плюс `berimor/plugins`; при
/// недоступности — временный каталог (не отказ, деградация только по
/// постоянству между запусками — тот же выбор, что `verify.rs::
/// trust_root_cache_dir`).
fn plugins_root_dir() -> PathBuf {
    dirs::data_dir()
        .or_else(dirs::config_dir)
        .map(|d| d.join("berimor").join("plugins"))
        .unwrap_or_else(|| std::env::temp_dir().join("berimor-plugins"))
}

#[allow(clippy::too_many_arguments)]
pub fn run(
    config: &Config,
    repo: &str,
    resume: &Option<String>,
    signer_workflow: Option<&str>,
    allowed_ref: Option<&str>,
    capability_ceiling: Option<&str>,
) -> Result<(), PluginInstallRunError> {
    // Независимое ревью (MINOR-4) — см. тот же фикс в self_update.rs.
    if resume.as_deref() == Some(TRUST_LIST_INSTANCE_ID) {
        return Err(PluginInstallRunError::ReservedInstanceId(
            TRUST_LIST_INSTANCE_ID.to_string(),
        ));
    }
    let process = parser::parse(PROCESS_YAML)
        .map_err(|err| PluginInstallRunError::ParseProcess(err.to_string()))?;

    let storage = Arc::new(SqliteEventLog::open(&config.storage_path).map_err(|err| {
        PluginInstallRunError::OpenStorage {
            path: config.storage_path.clone(),
            reason: err.to_string(),
        }
    })?);

    let mut instance = match resume {
        Some(id) => {
            let id = ProcessInstanceId(id.clone());
            let recovered = engine::recover(storage.as_ref(), process, id)?;
            println!(
                "[berimor] восстановлен инстанс plugin-install {} (шаг: {:?})",
                recovered.id.0, recovered.current_step
            );
            recovered
        }
        None => {
            let ceiling: Vec<String> = capability_ceiling
                .unwrap_or("")
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect();
            let input = json!({"local": {
                "repo": repo,
                "proposed_signer_identity": signer_workflow,
                "proposed_allowed_ref": allowed_ref.unwrap_or("v*.*.*"),
                "proposed_capability_ceiling": ceiling,
            }});
            let id = ProcessInstanceId(new_plugin_install_instance_id());
            let instance = engine::instantiate(storage.as_ref(), id, process, input)?;
            println!("[berimor] создан инстанс plugin-install {}", instance.id.0);
            instance
        }
    };

    let dispatch = PluginInstallDispatch::new(Arc::clone(&storage), plugins_root_dir())
        .map_err(PluginInstallRunError::Dispatch)?;

    let workspace_root = std::env::current_dir()
        .and_then(|p| p.canonicalize())
        .unwrap_or_else(|_| PathBuf::from("."));
    let gate = StandardCapability::new(workspace_root, plugin_install_tool_policies());
    let confirmer = TerminalConfirmer;

    let executor = PluginInstallExecutor {
        gate: &gate,
        mode: config.confirmation_mode,
        confirmer: &confirmer,
        dispatch: &dispatch,
    };

    loop {
        match engine::run(storage.as_ref(), &executor, &mut instance)? {
            engine::RunOutcome::Finished => {
                println!("[berimor] установка плагина завершена");
                println!(
                    "{}",
                    serde_json::to_string_pretty(&instance.state).expect("состояние сериализуемо")
                );
                return Ok(());
            }
            engine::RunOutcome::AwaitingHuman { step_id, reason } => {
                let resolved_reason = interpolate(&reason, &instance.state);
                let _ = storage.append(Event::new(
                    instance.id.clone(),
                    instance.process.version,
                    EventKind::HumanGateOpened {
                        reason: resolved_reason.clone(),
                    },
                    Value::Null,
                ));
                if !ask_human(&step_id, &resolved_reason) {
                    println!(
                        "[berimor] остановлено на human_gate '{step_id}'; возобновить: berimor plugin install {repo} --resume {}",
                        instance.id.0
                    );
                    return Err(PluginInstallRunError::HumanDeclined);
                }
                let _ = storage.append(Event::new(
                    instance.id.clone(),
                    instance.process.version,
                    EventKind::HumanGateResolved,
                    Value::Null,
                ));
            }
        }
    }
}

fn new_plugin_install_instance_id() -> String {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("plugin-install-{ms}-{}", std::process::id())
}

/// Фиксированная capability-политика — тот же принцип, что
/// `self_update::self_update_tool_policies`: список не пользовательский,
/// хардкод, не конфиг. `plugin.install` — единственная мутирующая
/// операция (перемещение файлов на диск + возможная запись в доверенный
/// список).
pub fn plugin_install_tool_policies() -> HashMap<String, ToolPolicy> {
    let mut policies = HashMap::new();
    for tool in [
        "trust.check_repo",
        "github.get_latest_release",
        "platform.resolve_plugin_asset",
        "github.download_release_asset",
        "crypto.verify_plugin_signature",
        "plugin.extract_and_read_manifest",
        "plugin_install.fail",
        "plugin_install.noop",
    ] {
        policies.insert(
            tool.to_string(),
            ToolPolicy {
                mutates: Some(false),
                ..Default::default()
            },
        );
    }
    policies.insert(
        "plugin.install".to_string(),
        ToolPolicy {
            mutates: Some(true),
            ..Default::default()
        },
    );
    policies
}

pub struct PluginInstallExecutor<'a> {
    pub gate: &'a dyn CapabilityGate,
    pub mode: ConfirmationMode,
    pub confirmer: &'a dyn ConfirmationHandler,
    pub dispatch: &'a dyn ToolDispatch,
}

impl engine::StepExecutor for PluginInstallExecutor<'_> {
    fn execute(&self, step: &Step, state: &Value) -> Result<Patch, ExecutorError> {
        match &step.kind {
            StepKind::Tool { tool, args } => tool_only::execute(
                &step.id,
                tool,
                args,
                state,
                self.dispatch,
                self.gate,
                self.mode,
                self.confirmer,
            )
            .map_err(|err| ExecutorError {
                step_id: step.id.clone(),
                reason: err.to_string(),
            }),
            other => Err(ExecutorError {
                step_id: step.id.clone(),
                reason: format!(
                    "тип шага не поддержан в plugin-install (только tool — остальные типы разрешает граф, не исполнитель): {other:?}"
                ),
            }),
        }
    }
}

pub struct PluginInstallDispatch {
    client: Client,
    storage: Arc<SqliteEventLog>,
    plugins_root: PathBuf,
    api_base: String,
    download_base: String,
    /// См. `self_update::SelfUpdateDispatch::gate_network` — тот же
    /// test-only обход гейта для локального тестового сервера.
    gate_network: bool,
}

impl PluginInstallDispatch {
    pub fn new(storage: Arc<SqliteEventLog>, plugins_root: PathBuf) -> Result<Self, String> {
        let client = Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|err| err.to_string())?;
        Ok(Self {
            client,
            storage,
            plugins_root,
            api_base: "https://api.github.com".to_string(),
            download_base: "https://github.com".to_string(),
            gate_network: true,
        })
    }

    #[cfg(test)]
    fn with_bases(
        storage: Arc<SqliteEventLog>,
        plugins_root: PathBuf,
        api_base: String,
        download_base: String,
    ) -> Self {
        let client = Client::builder().timeout(REQUEST_TIMEOUT).build().unwrap();
        Self {
            client,
            storage,
            plugins_root,
            api_base,
            download_base,
            gate_network: false,
        }
    }
}

impl ToolDispatch for PluginInstallDispatch {
    fn call(&self, tool: &str, args: &Value) -> Result<Value, DispatchError> {
        let result: Result<Value, String> = match tool {
            "trust.check_repo" => (|| {
                let repo = required_str(args, "repo")?;
                let proposed_signer_identity = args
                    .get("proposed_signer_identity")
                    .and_then(|v| v.as_str());
                let proposed_allowed_ref =
                    args.get("proposed_allowed_ref").and_then(|v| v.as_str());
                let proposed_capability_ceiling = string_array(args, "proposed_capability_ceiling");
                check_trust(
                    self.storage.as_ref(),
                    repo,
                    proposed_signer_identity,
                    proposed_allowed_ref,
                    proposed_capability_ceiling.as_deref(),
                )
            })(),
            "github.get_latest_release" => (|| {
                if self.gate_network {
                    require_network(&self.api_base)?;
                }
                let repo = required_str(args, "repo")?;
                let allowed_ref = required_str(args, "allowed_ref")?;
                get_latest_release(&self.client, &self.api_base, repo, allowed_ref)
            })(),
            "platform.resolve_plugin_asset" => (|| {
                let repo = required_str(args, "repo")?;
                let version = required_str(args, "version")?;
                resolve_plugin_asset(repo, version)
            })(),
            "github.download_release_asset" => (|| {
                if self.gate_network {
                    require_network(&self.download_base)?;
                }
                let repo = required_str(args, "repo")?;
                let version = required_str(args, "version")?;
                let asset_name = required_str(args, "asset_name")?;
                let dest_dir = std::env::temp_dir()
                    .join(format!("berimor-plugin-install-{}", std::process::id()));
                download_plugin_asset(
                    &self.client,
                    &self.download_base,
                    repo,
                    version,
                    asset_name,
                    &dest_dir,
                )
            })(),
            "crypto.verify_plugin_signature" => (|| {
                let archive_path = required_str(args, "archive_path")?;
                let repo = required_str(args, "repo")?;
                let signer_identity = required_str(args, "signer_identity")?;
                Ok(verify_plugin_signature(
                    Path::new(archive_path),
                    repo,
                    signer_identity,
                ))
            })(),
            "plugin.extract_and_read_manifest" => (|| {
                let archive_path = required_str(args, "archive_path")?;
                let allowed_ceiling =
                    string_array(args, "allowed_capability_ceiling").unwrap_or_default();
                extract_and_read_manifest(Path::new(archive_path), &allowed_ceiling)
            })(),
            "plugin.install" => (|| {
                let extract_dir = required_str(args, "extract_dir")?;
                let name = required_str(args, "name")?;
                let trusted = args
                    .get("trusted")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let repo = required_str(args, "repo")?;
                let signer_identity = required_str(args, "signer_identity")?;
                let allowed_ref = required_str(args, "allowed_ref")?;
                let capability_ceiling =
                    string_array(args, "capability_ceiling").unwrap_or_default();
                install_plugin(
                    self.storage.as_ref(),
                    &self.plugins_root,
                    Path::new(extract_dir),
                    name,
                    trusted,
                    repo,
                    signer_identity,
                    allowed_ref,
                    &capability_ceiling,
                )
            })(),
            "plugin_install.fail" => plugin_install_fail(args),
            "plugin_install.noop" => Ok(json!({})),
            other => Err(format!("неизвестный plugin-install-инструмент: {other}")),
        };
        result.map_err(|reason| DispatchError {
            tool: tool.to_string(),
            reason,
        })
    }
}

fn required_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("отсутствует обязательный аргумент '{key}'"))
}

fn string_array(args: &Value, key: &str) -> Option<Vec<String>> {
    args.get(key)?.as_array().map(|arr| {
        arr.iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect()
    })
}

fn host_of(base_url: &str) -> &str {
    base_url
        .split("://")
        .nth(1)
        .unwrap_or(base_url)
        .split('/')
        .next()
        .unwrap_or_default()
        .split(':')
        .next()
        .unwrap_or_default()
}

/// Тот же паттерн, что `self_update.rs::require_network` — см. его
/// doc-комментарий для полного обоснования (константные хосты GitHub, не
/// конфигурируемый пользователем `base_url`).
fn require_network(base_url: &str) -> Result<(), String> {
    match net_gate::check_host(host_of(base_url), 443) {
        NetworkDecision::Allow => Ok(()),
        NetworkDecision::ConfirmRequired { reason } => Err(format!(
            "сетевой гейт: {reason} (plugin-install обращается только к github.com/api.github.com)"
        )),
    }
}

/// Свёртка доверенного списка (D5) по конкретному `repo`. Уже доверен —
/// используется ЗАПИСЬ (запрос на расширение ceiling — отдельное
/// подтверждение, не эта функция); не доверен — обязателен
/// `proposed_signer_identity` (TOFU, см. doc-комментарий модуля), иначе
/// явный отказ, не молчаливый пропуск проверки подписи.
fn check_trust(
    storage: &dyn EventLog,
    repo: &str,
    proposed_signer_identity: Option<&str>,
    proposed_allowed_ref: Option<&str>,
    proposed_capability_ceiling: Option<&[String]>,
) -> Result<Value, String> {
    let events = storage
        .replay(&ProcessInstanceId(TRUST_LIST_INSTANCE_ID.to_string()))
        .map_err(|err| err.to_string())?;
    let list = fold_trust_list(&events);
    if let Some(entry) = list.get(repo) {
        return Ok(json!({
            "trusted": true,
            "signer_identity": entry.signer_identity,
            "allowed_ref": entry.allowed_ref,
            "capability_ceiling": entry.capability_ceiling,
        }));
    }
    let signer_identity = proposed_signer_identity
        .filter(|identity| is_plausible_signer_identity(identity))
        .ok_or_else(|| {
            format!(
                "репозиторий '{repo}' не в доверенном списке — для первой установки укажите непустой --signer-workflow вида 'https://github.com/<owner>/<repo>/.github/workflows/<file>.yml@' (и, при необходимости, --capability-ceiling/--allowed-ref)"
            )
        })?;
    Ok(json!({
        "trusted": false,
        "signer_identity": signer_identity,
        "allowed_ref": proposed_allowed_ref.unwrap_or("v*.*.*"),
        "capability_ceiling": proposed_capability_ceiling.unwrap_or(&[]),
    }))
}

#[derive(serde::Deserialize)]
struct GitHubRelease {
    tag_name: String,
}

fn fetch_latest_release(
    client: &Client,
    api_base: &str,
    repo: &str,
) -> Result<GitHubRelease, String> {
    let url = format!("{api_base}/repos/{repo}/releases/latest");
    let response = client
        .get(&url)
        .header("User-Agent", "berimor-plugin-install")
        .send()
        .map_err(|err| err.to_string())?;
    if !response.status().is_success() {
        return Err(format!("GitHub API вернул {}", response.status()));
    }
    response
        .json::<GitHubRelease>()
        .map_err(|err| err.to_string())
}

/// Совпадение с `allowed_ref` — паттерн из записи доверенного списка
/// (`deployment.md` §4: «allowed_ref: semver-паттерн тегов»), `*` —
/// подстановка любой (в т.ч. пустой) последовательности символов,
/// остальные символы — буквально. Стандартный двухуказательный
/// glob-алгоритм — не добавляем зависимость ради одной подстановки.
fn matches_ref_pattern(tag: &str, pattern: &str) -> bool {
    let tag: Vec<char> = tag.chars().collect();
    let pattern: Vec<char> = pattern.chars().collect();
    let (mut ti, mut pi) = (0usize, 0usize);
    let (mut star_pi, mut star_ti) = (None, 0usize);
    while ti < tag.len() {
        if pi < pattern.len() && (pattern[pi] == '*' || pattern[pi] == tag[ti]) {
            if pattern[pi] == '*' {
                star_pi = Some(pi);
                star_ti = ti;
                pi += 1;
            } else {
                ti += 1;
                pi += 1;
            }
        } else if let Some(sp) = star_pi {
            pi = sp + 1;
            star_ti += 1;
            ti = star_ti;
        } else {
            return false;
        }
    }
    while pi < pattern.len() && pattern[pi] == '*' {
        pi += 1;
    }
    pi == pattern.len()
}

/// Независимое ревью (MAJOR-2): `allowed_ref` — документированная граница
/// доверенного списка (`deployment.md` §4), но раньше нигде не
/// проверялась — `plugin install` ставил бы любой «latest» релиз
/// репозитория независимо от паттерна, который оператор явно ограничил
/// при `trust add`/TOFU. Проверка — здесь, а не отдельным шагом графа:
/// несовпадение обрывает процесс той же `Err`-из-tool'а веткой, что и
/// любая другая проверка self-update/plugin-install (I6, без нового
/// branch-узла в графе).
fn get_latest_release(
    client: &Client,
    api_base: &str,
    repo: &str,
    allowed_ref: &str,
) -> Result<Value, String> {
    let release = fetch_latest_release(client, api_base, repo)?;
    if !matches_ref_pattern(&release.tag_name, allowed_ref) {
        return Err(format!(
            "тег релиза '{}' не соответствует allowed_ref '{allowed_ref}' записи доверенного списка — установка отклонена",
            release.tag_name
        ));
    }
    let version = release.tag_name.trim_start_matches('v').to_string();
    Ok(json!({"version": version}))
}

/// Зеркало `self_update.rs::resolve_asset` — то же соглашение платформ,
/// имя строится из basename репозитория, не константы `"berimor"`.
fn resolve_plugin_asset(repo: &str, version: &str) -> Result<Value, String> {
    let basename = repo.rsplit('/').next().unwrap_or(repo);
    let (platform, ext) = match std::env::consts::OS {
        "linux" => ("linux", "tar.gz"),
        "macos" => ("darwin", "tar.gz"),
        "windows" => ("win32", "zip"),
        other => return Err(format!("неподдерживаемая платформа: {other}")),
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        other => return Err(format!("неподдерживаемая архитектура: {other}")),
    };
    Ok(json!({
        "asset_name": format!("{basename}-{version}-{platform}-{arch}.{ext}"),
    }))
}

fn download_file(client: &Client, url: &str, dest: &Path) -> Result<(), String> {
    let response = client.get(url).send().map_err(|err| err.to_string())?;
    if !response.status().is_success() {
        return Err(format!(
            "скачивание {url} завершилось HTTP {}",
            response.status()
        ));
    }
    let bytes = response.bytes().map_err(|err| err.to_string())?;
    std::fs::write(dest, &bytes).map_err(|err| err.to_string())?;
    Ok(())
}

/// Качает архив плагина И его `.sigstore.json`-бандл рядом — то же
/// соглашение имени, что `self_update.rs`/D2 (`verify.rs::bundle_path_for`).
fn download_plugin_asset(
    client: &Client,
    base_url: &str,
    repo: &str,
    version: &str,
    asset_name: &str,
    dest_dir: &Path,
) -> Result<Value, String> {
    std::fs::create_dir_all(dest_dir).map_err(|err| err.to_string())?;

    let archive_path = dest_dir.join(asset_name);
    let archive_url = format!("{base_url}/{repo}/releases/download/v{version}/{asset_name}");
    download_file(client, &archive_url, &archive_path)?;

    let sidecar_name = format!("{asset_name}.sigstore.json");
    let sidecar_path = dest_dir.join(&sidecar_name);
    let sidecar_url = format!("{base_url}/{repo}/releases/download/v{version}/{sidecar_name}");
    download_file(client, &sidecar_url, &sidecar_path)?;

    Ok(json!({"archive_path": archive_path.display().to_string()}))
}

fn verify_plugin_signature(archive_path: &Path, repo: &str, signer_identity: &str) -> Value {
    match crate::verify::verify_artifact_with_identity(archive_path, repo, signer_identity) {
        Ok(()) => json!({"ok": true}),
        Err(err) => json!({"ok": false, "error": err.to_string()}),
    }
}

/// Тот же паттерн, что `self_update.rs::extract_archive` — нативные
/// средства платформы (`tar`/`Expand-Archive`), не новая зависимость.
fn extract_archive(archive_path: &Path, dest_dir: &Path) -> Result<(), String> {
    let status = if cfg!(windows) {
        let escaped = |p: &Path| format!("'{}'", p.display().to_string().replace('\'', "''"));
        std::process::Command::new("powershell")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                &format!(
                    "Expand-Archive -LiteralPath {} -DestinationPath {} -Force",
                    escaped(archive_path),
                    escaped(dest_dir)
                ),
            ])
            .status()
    } else {
        std::process::Command::new("tar")
            .arg("-xzf")
            .arg(archive_path)
            .arg("-C")
            .arg(dest_dir)
            .status()
    }
    .map_err(|err| format!("не удалось запустить распаковку: {err}"))?;
    if !status.success() {
        return Err(format!("распаковка завершилась с кодом {status}"));
    }
    Ok(())
}

/// Распаковывает архив плагина, читает `manifest.yaml` изнутри
/// (`berimor_capability::plugin::load_manifest` — тот же формат, что
/// установленный плагин будет использовать в рантайме, S6/T3), сравнивает
/// заявленный `capability_ceiling` с разрешённым (подмножество —
/// `deployment.md` §6: манифест не может запросить больше потолка записи
/// доверенного списка).
fn extract_and_read_manifest(
    archive_path: &Path,
    allowed_ceiling: &[String],
) -> Result<Value, String> {
    let extract_dir = {
        let mut dir = archive_path.as_os_str().to_owned();
        dir.push(".extracted");
        PathBuf::from(dir)
    };
    std::fs::create_dir_all(&extract_dir).map_err(|err| err.to_string())?;
    extract_archive(archive_path, &extract_dir)?;

    let manifest_path = extract_dir.join("manifest.yaml");
    let manifest = load_manifest(&manifest_path).map_err(|err| err.to_string())?;
    // Независимое ревью (MINOR-5): показываем человеку РАЗНИЦУ множеств,
    // не только сам факт превышения — `ceiling_review` в графе иначе не
    // даёт осознанно решить, что именно запрашивает манифест сверх нормы.
    let exceeding: Vec<String> = manifest
        .capability_ceiling
        .iter()
        .filter(|c| !allowed_ceiling.iter().any(|a| a == *c))
        .cloned()
        .collect();
    let within_ceiling = exceeding.is_empty();

    Ok(json!({
        "name": manifest.name,
        "capability_ceiling": manifest.capability_ceiling,
        "within_ceiling": within_ceiling,
        "exceeding_capability_ceiling": exceeding,
        "extract_dir": extract_dir.display().to_string(),
    }))
}

/// Финальный шаг: перемещает извлечённый плагин в изолированный каталог,
/// копирует манифест в каталог, который `PluginRegistry::load_dir`
/// сканирует (регистрация через файл на диске — `PluginRegistry` не
/// предоставляет метода `add`, только `load_dir` при старте, тот же выбор,
/// что и у `self_update.rs`'s не-существующего "runtime API" для
/// разблокировки — задокументированная, не забытая граница). Если
/// репозиторий был НЕ доверен до этой установки — дописывает
/// `TrustListChanged::Added` в тот же журнал, что и `berimor trust add`:
/// успешная установка с нового репозитория делает его доверенным на
/// будущее, не отдельным путём мимо D5.
/// Независимое ревью (CRITICAL-1): `name` — `manifest.name`, ПОЛНОСТЬЮ
/// контролируется автором плагина, `berimor_capability::plugin::
/// load_manifest` его не валидирует. Без этой проверки `plugins_root.
/// join("installed").join(name)` для `name = "../../../.ssh/
/// authorized_keys"` (или абсолютного пути — `PathBuf::join` с абсолютным
/// операндом ПОЛНОСТЬЮ заменяет базу) уходит за пределы `plugins_root`;
/// хуже того, `remove_dir_all` на уже существующем по этому «убежавшему»
/// пути выполнился бы ДО подмены — произвольное рекурсивное удаление, не
/// только запись. Это не защищено `deny.rs` (проверяет только аргументы с
/// ключами вроде `path`/`dir`, `name`/`extract_dir` не совпадают) и не
/// `jail` (в этом инструменте не вызывается вовсе, PoC независимого
/// ревью подтвердил выход за пределы через `PathBuf::join`). Действует
/// одинаково и для уже ДОВЕРЕННОГО репозитория — доверие удостоверяет
/// подписанта CI, не содержимое `manifest.yaml`, которое тот же
/// подписант полностью контролирует.
fn validate_plugin_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("имя плагина (manifest.name) не может быть пустым".to_string());
    }
    if name == "." || name == ".." {
        return Err(format!("имя плагина '{name}' недопустимо"));
    }
    let is_safe = name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.');
    if !is_safe || name.contains("..") || Path::new(name).is_absolute() {
        return Err(format!(
            "имя плагина '{name}' содержит недопустимые символы (разрешены только буквы/цифры/`-`/`_`/`.`, без `..` и без абсолютных путей) — отказ до любых операций с файловой системой"
        ));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn install_plugin(
    storage: &dyn EventLog,
    plugins_root: &Path,
    extract_dir: &Path,
    name: &str,
    trusted: bool,
    repo: &str,
    signer_identity: &str,
    allowed_ref: &str,
    capability_ceiling: &[String],
) -> Result<Value, String> {
    validate_plugin_name(name)?;
    let installed_dir = plugins_root.join("installed").join(name);
    if let Some(parent) = installed_dir.parent() {
        std::fs::create_dir_all(parent).map_err(|err| err.to_string())?;
    }
    if installed_dir.exists() {
        std::fs::remove_dir_all(&installed_dir).map_err(|err| err.to_string())?;
    }
    std::fs::rename(extract_dir, &installed_dir).map_err(|err| err.to_string())?;

    let manifests_dir = plugins_root.join("manifests");
    std::fs::create_dir_all(&manifests_dir).map_err(|err| err.to_string())?;
    std::fs::copy(
        installed_dir.join("manifest.yaml"),
        manifests_dir.join(format!("{name}.yaml")),
    )
    .map_err(|err| err.to_string())?;

    if !trusted {
        storage
            .append(Event::new(
                ProcessInstanceId(TRUST_LIST_INSTANCE_ID.to_string()),
                0,
                EventKind::TrustListChanged {
                    action: TrustListAction::Added,
                    repo: repo.to_string(),
                    allowed_ref: allowed_ref.to_string(),
                    signer_identity: signer_identity.to_string(),
                    capability_ceiling: capability_ceiling.to_vec(),
                },
                Value::Null,
            ))
            .map_err(|err| err.to_string())?;
    }

    Ok(json!({"installed_path": installed_dir.display().to_string()}))
}

/// Всегда `Err` — обрывает процесс, тот же принцип, что
/// `self_update.rs::self_update_fail` (I6: структурное свойство графа, не
/// проверка флага).
fn plugin_install_fail(args: &Value) -> Result<Value, String> {
    let reason = args
        .get("reason")
        .and_then(|v| v.as_str())
        .unwrap_or("установка плагина остановлена");
    Err(reason.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    fn uuid_like_suffix() -> String {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        format!(
            "{}-{}-{n}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        )
    }

    fn temp_work_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "berimor-plugin-install-{label}-{}",
            uuid_like_suffix()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn in_memory_storage() -> Arc<SqliteEventLog> {
        Arc::new(SqliteEventLog::open_in_memory().unwrap())
    }

    fn spawn_sequenced_server(
        responses: Vec<(&'static str, Vec<u8>)>,
    ) -> (String, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = std::thread::spawn(move || {
            for (status_line, body) in responses {
                let Ok((mut stream, _)) = listener.accept() else {
                    break;
                };
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf);
                let header = format!(
                    "HTTP/1.1 {status_line}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(header.as_bytes());
                let _ = stream.write_all(&body);
            }
        });
        (format!("http://{addr}"), handle)
    }

    #[cfg(unix)]
    fn pack_test_archive(dir_to_pack: &Path, archive_path: &Path) {
        let status = std::process::Command::new("tar")
            .arg("-czf")
            .arg(archive_path)
            .arg("-C")
            .arg(dir_to_pack)
            .arg(".")
            .status()
            .unwrap();
        assert!(status.success());
    }

    #[cfg(windows)]
    fn pack_test_archive(dir_to_pack: &Path, archive_path: &Path) {
        let escaped = |p: &Path| format!("'{}'", p.display().to_string().replace('\'', "''"));
        let status = std::process::Command::new("powershell")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                &format!(
                    "Compress-Archive -Path {}\\* -DestinationPath {} -Force",
                    escaped(dir_to_pack).trim_matches('\''),
                    escaped(archive_path)
                ),
            ])
            .status()
            .unwrap();
        assert!(status.success());
    }

    fn test_archive_extension() -> &'static str {
        if cfg!(windows) {
            "zip"
        } else {
            "tar.gz"
        }
    }

    #[test]
    fn resolve_plugin_asset_names_current_platform_from_repo_basename() {
        let result = resolve_plugin_asset("owner/my-plugin", "1.2.3").unwrap();
        let name = result["asset_name"].as_str().unwrap();
        assert!(name.starts_with("my-plugin-1.2.3-"));
        assert!(name.ends_with(".tar.gz") || name.ends_with(".zip"));
    }

    #[test]
    fn get_latest_release_reports_version_without_v_prefix() {
        let (base, handle) =
            spawn_sequenced_server(vec![("200 OK", br#"{"tag_name": "v2.0.0"}"#.to_vec())]);
        let client = Client::builder().timeout(REQUEST_TIMEOUT).build().unwrap();
        let result = get_latest_release(&client, &base, "owner/plugin", "v*.*.*").unwrap();
        assert_eq!(result["version"], "2.0.0");
        handle.join().unwrap();
    }

    #[test]
    fn get_latest_release_rejects_tag_outside_allowed_ref() {
        let (base, handle) =
            spawn_sequenced_server(vec![("200 OK", br#"{"tag_name": "v2.0.0"}"#.to_vec())]);
        let client = Client::builder().timeout(REQUEST_TIMEOUT).build().unwrap();
        let err = get_latest_release(&client, &base, "owner/plugin", "v1.*.*").unwrap_err();
        assert!(err.contains("не соответствует allowed_ref"));
        handle.join().unwrap();
    }

    #[test]
    fn matches_ref_pattern_supports_star_wildcard() {
        assert!(matches_ref_pattern("v1.2.3", "v1.*.*"));
        assert!(matches_ref_pattern("v1.2.3", "v*.*.*"));
        assert!(!matches_ref_pattern("v2.0.0", "v1.*.*"));
        assert!(matches_ref_pattern("anything", "*"));
        assert!(!matches_ref_pattern("v1.2.3", "v1.2"));
    }

    #[test]
    fn validate_plugin_name_rejects_path_traversal_and_absolute_paths() {
        assert!(validate_plugin_name("my-plugin").is_ok());
        assert!(validate_plugin_name("my_plugin.v2").is_ok());
        assert!(validate_plugin_name("").is_err());
        assert!(validate_plugin_name("..").is_err());
        assert!(validate_plugin_name("../../../etc/passwd").is_err());
        assert!(validate_plugin_name("../evil").is_err());
        assert!(validate_plugin_name("a/b").is_err());
        assert!(validate_plugin_name("a\\b").is_err());
        #[cfg(unix)]
        assert!(validate_plugin_name("/etc/cron.d/evil").is_err());
    }

    #[test]
    fn install_plugin_rejects_traversal_name_before_touching_the_filesystem() {
        let storage = in_memory_storage();
        let work = temp_work_dir("install-traversal");
        let extract_dir = work.join("extracted");
        std::fs::create_dir_all(&extract_dir).unwrap();
        std::fs::write(extract_dir.join("manifest.yaml"), "name: evil\n").unwrap();

        let plugins_root = work.join("plugins-root");
        let err = install_plugin(
            storage.as_ref(),
            &plugins_root,
            &extract_dir,
            "../../escaped",
            false,
            "owner/plugin",
            "identity",
            "v*.*.*",
            &[],
        )
        .unwrap_err();
        assert!(err.contains("недопустим"));
        // Ничего не должно было измениться на диске за пределами work/extracted.
        assert!(!plugins_root.exists());
        assert!(extract_dir.join("manifest.yaml").exists());

        std::fs::remove_dir_all(&work).ok();
    }

    #[test]
    fn download_plugin_asset_fetches_archive_and_sidecar() {
        let (base, handle) = spawn_sequenced_server(vec![
            ("200 OK", b"archive-bytes".to_vec()),
            ("200 OK", b"sidecar-bytes".to_vec()),
        ]);
        let client = Client::builder().timeout(REQUEST_TIMEOUT).build().unwrap();
        let dest_dir = temp_work_dir("download");
        let result = download_plugin_asset(
            &client,
            &base,
            "owner/plugin",
            "1.2.3",
            "plugin-1.2.3-linux-x64.tar.gz",
            &dest_dir,
        )
        .unwrap();
        let archive_path = result["archive_path"].as_str().unwrap();
        assert_eq!(std::fs::read(archive_path).unwrap(), b"archive-bytes");
        assert!(dest_dir
            .join("plugin-1.2.3-linux-x64.tar.gz.sigstore.json")
            .exists());
        handle.join().unwrap();
        std::fs::remove_dir_all(&dest_dir).ok();
    }

    #[test]
    fn check_trust_unknown_repo_without_proposed_identity_is_an_error() {
        let storage = in_memory_storage();
        let err = check_trust(storage.as_ref(), "owner/plugin", None, None, None).unwrap_err();
        assert!(err.contains("не в доверенном списке"));
    }

    #[test]
    fn check_trust_unknown_repo_with_proposed_identity_reports_untrusted_with_proposal() {
        let storage = in_memory_storage();
        let ceiling = vec!["net.http".to_string()];
        let result = check_trust(
            storage.as_ref(),
            "owner/plugin",
            Some("https://github.com/owner/plugin/.github/workflows/release.yml@"),
            Some("v*.*.*"),
            Some(&ceiling),
        )
        .unwrap();
        assert_eq!(result["trusted"], false);
        assert_eq!(
            result["signer_identity"],
            "https://github.com/owner/plugin/.github/workflows/release.yml@"
        );
        assert_eq!(result["capability_ceiling"], json!(["net.http"]));
    }

    #[test]
    fn check_trust_known_repo_uses_the_existing_entry_ignoring_proposed() {
        let storage = in_memory_storage();
        storage
            .append(Event::new(
                ProcessInstanceId(TRUST_LIST_INSTANCE_ID.to_string()),
                0,
                EventKind::TrustListChanged {
                    action: TrustListAction::Added,
                    repo: "owner/plugin".to_string(),
                    allowed_ref: "v1.*.*".to_string(),
                    signer_identity: "pinned-identity".to_string(),
                    capability_ceiling: vec!["net.http".to_string()],
                },
                Value::Null,
            ))
            .unwrap();

        let result = check_trust(
            storage.as_ref(),
            "owner/plugin",
            Some("attacker-supplied-identity"),
            None,
            None,
        )
        .unwrap();
        assert_eq!(result["trusted"], true);
        assert_eq!(result["signer_identity"], "pinned-identity");
        assert_eq!(result["allowed_ref"], "v1.*.*");
    }

    #[test]
    fn extract_and_read_manifest_reports_within_ceiling_true_when_subset() {
        let work = temp_work_dir("manifest-within");
        let pack_dir = work.join("pack");
        std::fs::create_dir_all(&pack_dir).unwrap();
        std::fs::write(
            pack_dir.join("manifest.yaml"),
            "name: my-plugin\ncapability_ceiling: [\"net.http\"]\n",
        )
        .unwrap();
        let archive_path = work.join(format!("plugin.{}", test_archive_extension()));
        pack_test_archive(&pack_dir, &archive_path);

        let allowed = vec!["net.http".to_string(), "fs.read".to_string()];
        let result = extract_and_read_manifest(&archive_path, &allowed).unwrap();
        assert_eq!(result["name"], "my-plugin");
        assert_eq!(result["within_ceiling"], true);

        std::fs::remove_dir_all(&work).ok();
    }

    #[test]
    fn extract_and_read_manifest_reports_within_ceiling_false_when_requesting_more() {
        let work = temp_work_dir("manifest-over");
        let pack_dir = work.join("pack");
        std::fs::create_dir_all(&pack_dir).unwrap();
        std::fs::write(
            pack_dir.join("manifest.yaml"),
            "name: my-plugin\ncapability_ceiling: [\"net.http\", \"fs.write\"]\n",
        )
        .unwrap();
        let archive_path = work.join(format!("plugin.{}", test_archive_extension()));
        pack_test_archive(&pack_dir, &archive_path);

        let allowed = vec!["net.http".to_string()];
        let result = extract_and_read_manifest(&archive_path, &allowed).unwrap();
        assert_eq!(result["within_ceiling"], false);
        assert_eq!(result["exceeding_capability_ceiling"], json!(["fs.write"]));

        std::fs::remove_dir_all(&work).ok();
    }

    #[test]
    fn install_plugin_moves_files_and_records_new_trust_when_untrusted() {
        let storage = in_memory_storage();
        let work = temp_work_dir("install");
        let extract_dir = work.join("extracted");
        std::fs::create_dir_all(&extract_dir).unwrap();
        std::fs::write(extract_dir.join("manifest.yaml"), "name: my-plugin\n").unwrap();
        std::fs::write(extract_dir.join("my-plugin"), "binary-content").unwrap();

        let plugins_root = work.join("plugins-root");
        let result = install_plugin(
            storage.as_ref(),
            &plugins_root,
            &extract_dir,
            "my-plugin",
            false,
            "owner/plugin",
            "identity",
            "v*.*.*",
            &["net.http".to_string()],
        )
        .unwrap();

        let installed_path = PathBuf::from(result["installed_path"].as_str().unwrap());
        assert!(installed_path.join("my-plugin").exists());
        assert!(plugins_root
            .join("manifests")
            .join("my-plugin.yaml")
            .exists());

        let events = storage
            .replay(&ProcessInstanceId(TRUST_LIST_INSTANCE_ID.to_string()))
            .unwrap();
        assert_eq!(events.len(), 1);

        std::fs::remove_dir_all(&work).ok();
    }

    #[test]
    fn install_plugin_does_not_touch_trust_list_when_already_trusted() {
        let storage = in_memory_storage();
        let work = temp_work_dir("install-trusted");
        let extract_dir = work.join("extracted");
        std::fs::create_dir_all(&extract_dir).unwrap();
        std::fs::write(extract_dir.join("manifest.yaml"), "name: my-plugin\n").unwrap();

        let plugins_root = work.join("plugins-root");
        install_plugin(
            storage.as_ref(),
            &plugins_root,
            &extract_dir,
            "my-plugin",
            true,
            "owner/plugin",
            "identity",
            "v*.*.*",
            &[],
        )
        .unwrap();

        let events = storage
            .replay(&ProcessInstanceId(TRUST_LIST_INSTANCE_ID.to_string()))
            .unwrap();
        assert!(events.is_empty());

        std::fs::remove_dir_all(&work).ok();
    }

    #[test]
    fn plugin_install_fail_is_always_an_error() {
        let result = plugin_install_fail(&json!({"reason": "verify failed"}));
        assert_eq!(result.unwrap_err(), "verify failed");
    }

    struct PanicIfAsked;
    impl ConfirmationHandler for PanicIfAsked {
        fn confirm(
            &self,
            _action: &berimor_types::capability::ProposedAction,
            _reason: &str,
        ) -> bool {
            panic!("plugin-install golden-тест не должен запрашивать подтверждение")
        }
    }

    /// Контрактный тест на граф целиком по пути «уже доверенный репозиторий»
    /// — `trust_gate:true` минует `new_repo_review` (human_gate),
    /// `resolve_release`→`resolve_asset`→`download` реально ходят на
    /// локальный сервер и реально распаковывают архив. Дойти до `Finished`
    /// здесь НЕ получится без настоящего sigstore-бандла (которого у теста
    /// нет — тот же класс ограничения, что у `self_update.rs`'s golden-
    /// тестов): `verify` легитимно проваливается на реальной
    /// криптопроверке, `verify_gate` уводит на `fail_install` — это и
    /// проверяется. Логика `install_plugin`/списывания в доверенный
    /// список для НОВОГО репозитория покрыта отдельно юнит-тестами
    /// `check_trust`/`install_plugin` выше — через граф её без настоящей
    /// подписи не проверить.
    #[test]
    fn golden_process_trusted_repo_verify_gate_routes_to_fail_install() {
        let work = temp_work_dir("golden-happy");
        let pack_dir = work.join("pack");
        std::fs::create_dir_all(&pack_dir).unwrap();
        std::fs::write(
            pack_dir.join("manifest.yaml"),
            "name: my-plugin\ncapability_ceiling: [\"net.http\"]\n",
        )
        .unwrap();
        std::fs::write(
            pack_dir.join(if cfg!(windows) {
                "my-plugin.exe"
            } else {
                "my-plugin"
            }),
            b"bin",
        )
        .unwrap();
        let archive_path = work.join(format!("archive.{}", test_archive_extension()));
        pack_test_archive(&pack_dir, &archive_path);
        let archive_bytes = std::fs::read(&archive_path).unwrap();

        let (base, handle) = spawn_sequenced_server(vec![
            ("200 OK", br#"{"tag_name": "v1.0.0"}"#.to_vec()),
            ("200 OK", archive_bytes),
            ("200 OK", b"sidecar-not-used-because-verify-fails".to_vec()),
        ]);

        let storage = in_memory_storage();
        storage
            .append(Event::new(
                ProcessInstanceId(TRUST_LIST_INSTANCE_ID.to_string()),
                0,
                EventKind::TrustListChanged {
                    action: TrustListAction::Added,
                    repo: "owner/plugin".to_string(),
                    allowed_ref: "v*.*.*".to_string(),
                    signer_identity: "identity-that-will-not-verify".to_string(),
                    capability_ceiling: vec!["net.http".to_string()],
                },
                Value::Null,
            ))
            .unwrap();

        let process = parser::parse(PROCESS_YAML).unwrap();
        let input = json!({"local": {
            "repo": "owner/plugin",
            "proposed_signer_identity": Value::Null,
            "proposed_allowed_ref": "v*.*.*",
            "proposed_capability_ceiling": Vec::<String>::new(),
        }});
        let id = ProcessInstanceId("golden-plugin-happy".to_string());
        let mut instance = engine::instantiate(storage.as_ref(), id, process, input).unwrap();

        let plugins_root = work.join("plugins-root");
        let dispatch = PluginInstallDispatch::with_bases(
            Arc::clone(&storage),
            plugins_root,
            base.clone(),
            base,
        );
        let gate = StandardCapability::new(std::env::temp_dir(), plugin_install_tool_policies());
        let confirmer = PanicIfAsked;
        let executor = PluginInstallExecutor {
            gate: &gate,
            mode: ConfirmationMode::Off,
            confirmer: &confirmer,
            dispatch: &dispatch,
        };

        // Реальная verify_artifact_with_identity здесь провалится (нет
        // настоящего sigstore-бандла) — граф обязан дойти именно до
        // fail_install, не запаниковать и не зависнуть; это же доказывает,
        // что весь путь check_trust→download→verify→verify_gate реально
        // прошёл через настоящий локальный сервер и настоящую распаковку.
        let outcome = engine::run(storage.as_ref(), &executor, &mut instance);
        assert!(outcome.is_err(), "ожидался обрыв на fail_install (verify не пройдёт без настоящего sigstore-бандла), получено {outcome:?}");

        handle.join().unwrap();
        std::fs::remove_dir_all(&work).ok();
    }

    #[test]
    fn golden_process_untrusted_repo_without_signer_workflow_flag_is_rejected_before_any_network_call(
    ) {
        let storage = in_memory_storage();
        let process = parser::parse(PROCESS_YAML).unwrap();
        let input = json!({"local": {
            "repo": "owner/never-trusted",
            "proposed_signer_identity": Value::Null,
            "proposed_allowed_ref": "v*.*.*",
            "proposed_capability_ceiling": Vec::<String>::new(),
        }});
        let id = ProcessInstanceId("golden-plugin-no-identity".to_string());
        let mut instance = engine::instantiate(storage.as_ref(), id, process, input).unwrap();

        let plugins_root = temp_work_dir("golden-no-identity-plugins");
        // base-урлы намеренно указывают в никуда — тест доказывает, что
        // до сети дело не доходит вовсе (check_trust отказывает первым).
        let dispatch = PluginInstallDispatch::with_bases(
            Arc::clone(&storage),
            plugins_root.clone(),
            "http://127.0.0.1:1".to_string(),
            "http://127.0.0.1:1".to_string(),
        );
        let gate = StandardCapability::new(std::env::temp_dir(), plugin_install_tool_policies());
        let confirmer = PanicIfAsked;
        let executor = PluginInstallExecutor {
            gate: &gate,
            mode: ConfirmationMode::Off,
            confirmer: &confirmer,
            dispatch: &dispatch,
        };

        let outcome = engine::run(storage.as_ref(), &executor, &mut instance);
        assert!(outcome.is_err());
        std::fs::remove_dir_all(&plugins_root).ok();
    }
}
