//! `agent-self-update` (ROADMAP D4) — процесс Process Engine на выделенном,
//! минимальном исполнителе/диспетче, ОТДЕЛЬНОМ от `run.rs::build_executor_bundle`.
//!
//! Источник: `docs/arch/deployment.md` §5. Иллюстративный YAML в документе
//! не исполним буквально текущим движком — три структурных отличия,
//! обнаруженные при реализации (не теоретизирование, см. `docs/ROADMAP.md`
//! §14 D4 для полного разбора):
//!
//! 1. `StepKind::Branch.on` — ОДИН путь состояния (`state_path::resolve`),
//!    не выражение сравнения двух путей. Сравнение версий вычисляет сам
//!    `registry.get_latest` (крейт `semver`) и кладёт готовые булевы поля
//!    `is_newer`/`is_major_bump` — граф проверяет уже готовый bool.
//! 2. `"done"` НЕ специальный таргет — `NextStep::Finished` наступает
//!    только когда для текущего шага нет следующего элемента в плоском
//!    списке `steps` (`graph.rs::next_step`). Граф процесса
//!    (`fixtures/golden/processes/agent-self-update.yaml`) физически
//!    проектируется так, что ровно один общий терминальный шаг — последний
//!    элемент массива.
//! 3. `StepKind::Checkpoint` — это аудит-снапшот состояния Process Engine
//!    (`storage.write_snapshot`), не резервная копия бинарного файла.
//!    Бэкап/откат САМОГО БИНАРНИКА — целиком внутри `fs.
//!    atomic_replace_binary`/`fs.restore_from_checkpoint` (переименование
//!    файла в сторону перед заменой = и бэкап, и подготовка отката одним
//!    действием) — движок для этого не трогается вообще.
//!
//! **Периметр безопасности:** инструменты этого модуля (в т.ч. замена
//! исполняемого файла) видны ТОЛЬКО процессу `agent-self-update` —
//! `SelfUpdateDispatch` не часть `CompositeToolDispatch`, которым
//! пользуется обычный `berimor run <любой.yaml>`. Пользовательский процесс
//! не может вызвать `fs.atomic_replace_binary`, просто назвав его в своём
//! YAML. `SelfUpdateExecutor` поддерживает только `StepKind::Tool` — у
//! self-update-процесса нет llm_structured/agent_step/codeact шагов.
//!
//! **Сознательное сужение:** реализован только канал `stable`
//! (`GET /repos/{}/releases/latest`) — `release.yml` сегодня не производит
//! пререлизных тегов, придумывать несуществующую схему тегирования для
//! `beta`/`canary` раньше, чем она появится в пайплайне, не имеет смысла.

use berimor_capability::confirm::{StandardCapability, ToolPolicy};
use berimor_capability::net_gate::{self, NetworkDecision};
use berimor_capability::CapabilityGate;
use berimor_executors::tool_only::{self, ConfirmationHandler, DispatchError, ToolDispatch};
use berimor_process_engine::{
    engine::{self, ExecutorError},
    parser,
};
use berimor_storage::{EventLog, SqliteEventLog};
use berimor_types::capability::ConfirmationMode;
use berimor_types::event::{Event, EventKind, ProcessInstanceId};
use berimor_types::step::{Patch, Step, StepKind};
use reqwest::blocking::Client;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::config::Config;
use crate::run::{ask_human, interpolate, TerminalConfirmer};

/// Встроенный граф процесса — не читается с диска пользователя: набор
/// tool-инструментов фиксирован (см. doc-комментарий модуля), процесс
/// должен быть ровно тем, для которого собран `SelfUpdateDispatch`.
const PROCESS_YAML: &str =
    include_str!("../../../fixtures/golden/processes/agent-self-update.yaml");

#[derive(Debug, thiserror::Error)]
pub enum SelfUpdateRunError {
    #[error("не удалось разобрать встроенный процесс agent-self-update: {0}")]
    ParseProcess(String),
    #[error("не удалось открыть журнал {path}: {reason}")]
    OpenStorage { path: PathBuf, reason: String },
    #[error("движок: {0}")]
    Engine(#[from] engine::EngineError),
    #[error("не удалось определить путь к текущему исполняемому файлу: {0}")]
    CurrentExe(String),
    #[error("не удалось собрать self-update диспетчер: {0}")]
    Dispatch(String),
    #[error("выполнение остановлено на шаге human_gate: человек отклонил продолжение")]
    HumanDeclined,
}

/// Точка входа `berimor self-update` (`Command::SelfUpdate`, main.rs).
/// Зеркалит структуру `run.rs::run()` (тот же цикл human_gate,
/// `instantiate`/`recover` на том же `config.storage_path` — отдельное
/// хранилище self-update не нужно), но с `SelfUpdateExecutor`/
/// `SelfUpdateDispatch` вместо `CliExecutor`/`ExecutorBundle` — у процесса
/// нет llm_structured/agent_step/codeact шагов, ModelPool не нужен.
pub fn run(config: &Config, resume: &Option<String>) -> Result<(), SelfUpdateRunError> {
    let process = parser::parse(PROCESS_YAML)
        .map_err(|err| SelfUpdateRunError::ParseProcess(err.to_string()))?;

    let storage = SqliteEventLog::open(&config.storage_path).map_err(|err| {
        SelfUpdateRunError::OpenStorage {
            path: config.storage_path.clone(),
            reason: err.to_string(),
        }
    })?;

    let mut instance = match resume {
        Some(id) => {
            let id = ProcessInstanceId(id.clone());
            let recovered = engine::recover(&storage, process, id)?;
            println!(
                "[berimor] восстановлен инстанс self-update {} (шаг: {:?})",
                recovered.id.0, recovered.current_step
            );
            recovered
        }
        None => {
            let input = json!({"local": {
                "version": env!("CARGO_PKG_VERSION"),
                "channel": update_channel_str(config.update_channel),
            }});
            let id = ProcessInstanceId(new_self_update_instance_id());
            let instance = engine::instantiate(&storage, id, process, input)?;
            println!("[berimor] создан инстанс self-update {}", instance.id.0);
            instance
        }
    };

    let current_exe =
        std::env::current_exe().map_err(|err| SelfUpdateRunError::CurrentExe(err.to_string()))?;
    let dispatch = SelfUpdateDispatch::new(current_exe).map_err(SelfUpdateRunError::Dispatch)?;

    let workspace_root = std::env::current_dir()
        .and_then(|p| p.canonicalize())
        .unwrap_or_else(|_| PathBuf::from("."));
    let gate = StandardCapability::new(workspace_root, self_update_tool_policies());
    let confirmer = TerminalConfirmer;

    let executor = SelfUpdateExecutor {
        gate: &gate,
        mode: config.confirmation_mode,
        confirmer: &confirmer,
        dispatch: &dispatch,
    };

    loop {
        match engine::run(&storage, &executor, &mut instance)? {
            engine::RunOutcome::Finished => {
                println!("[berimor] self-update завершён");
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
                        "[berimor] остановлено на human_gate '{step_id}'; возобновить: berimor self-update --resume {}",
                        instance.id.0
                    );
                    return Err(SelfUpdateRunError::HumanDeclined);
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

/// `config.update_channel` решает, какой канал запрашивается у
/// `registry.get_latest` (I1: что обновлять — решает код, не эвристика) —
/// `beta`/`canary` дойдут до явного отказа "канал ещё не поддержан"
/// (`registry_get_latest`), не будут молча заменены на `stable`.
fn update_channel_str(channel: crate::config::UpdateChannel) -> &'static str {
    use crate::config::UpdateChannel;
    match channel {
        UpdateChannel::Stable => "stable",
        UpdateChannel::Beta => "beta",
        UpdateChannel::Canary => "canary",
    }
}

fn new_self_update_instance_id() -> String {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("self-update-{ms}-{}", std::process::id())
}

/// Тот же репозиторий, что `crate::verify::RELEASE_REPOSITORY` и
/// `bootstrap/src/download.ts::RELEASE_REPOSITORY` — три независимых
/// константы вместо одной общей ради простоты (Rust/TS не делят код,
/// а внутри `berimor-cli` `verify.rs` не экспортирует свою константу).
const RELEASE_REPOSITORY: &str = "devpilgrin/berimor";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Фиксированная capability-политика self-update-инструментов — список не
/// пользовательский (в отличие от `config.tool_stubs`), поэтому хардкод, не
/// конфиг. Замена/откат бинарника — `mutates: true` (в режиме `deny`
/// заблокировано безусловно, в `smart`/`manual` — подтверждение); чтения и
/// сетевые вызовы — `mutates: false`, не требуют подтверждения.
pub fn self_update_tool_policies() -> HashMap<String, ToolPolicy> {
    let mut policies = HashMap::new();
    for tool in [
        "registry.get_latest",
        "platform.resolve_asset",
        "github.download_release_asset",
        "crypto.verify_signature",
        "agent.self_check",
        "registry.record_version",
        "self_update.fail",
        "self_update.noop",
    ] {
        policies.insert(
            tool.to_string(),
            ToolPolicy {
                mutates: Some(false),
                ..Default::default()
            },
        );
    }
    for tool in ["fs.atomic_replace_binary", "fs.restore_from_checkpoint"] {
        policies.insert(
            tool.to_string(),
            ToolPolicy {
                mutates: Some(true),
                ..Default::default()
            },
        );
    }
    policies
}

/// Минимальный `StepExecutor` для self-update — поддерживает только
/// `StepKind::Tool` (см. doc-комментарий модуля). `Branch`/`HumanGate`/
/// `Checkpoint` разрешает сам движок (`graph.rs`/`engine.rs`), сюда не
/// попадают вообще — тот же принцип, что и у `CliExecutor` (`run.rs`).
pub struct SelfUpdateExecutor<'a> {
    pub gate: &'a dyn CapabilityGate,
    pub mode: ConfirmationMode,
    pub confirmer: &'a dyn ConfirmationHandler,
    pub dispatch: &'a dyn ToolDispatch,
}

impl engine::StepExecutor for SelfUpdateExecutor<'_> {
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
                    "тип шага не поддержан в agent-self-update (только tool — остальные типы разрешает граф, не исполнитель): {other:?}"
                ),
            }),
        }
    }
}

/// `ToolDispatch` c восемью именами инструментов self-update. Сетевые хосты
/// (`api_base`/`download_base`) — поля, не константы: продакшен всегда
/// использует реальный GitHub, тесты подставляют локальный тестовый
/// HTTP-сервер (`net_gate` в проде блокирует приватные адреса — тесты не
/// проходят через `call()`/гейт вовсе, обращаются к свободным функциям
/// ниже напрямую, что и проверяет их логику независимо от гейта, у
/// которого уже есть собственный набор тестов в `net_gate.rs`).
pub struct SelfUpdateDispatch {
    client: Client,
    current_exe: PathBuf,
    api_base: String,
    download_base: String,
    /// `false` только из тестового конструктора [`Self::with_bases`] — там
    /// `api_base`/`download_base` намеренно указывают на локальный тестовый
    /// сервер (`127.0.0.1`), который `net_gate` иначе классифицировал бы
    /// как приватный адрес и блокировал безусловно. Продакшен-конструктор
    /// [`Self::new`] всегда `true`: два захардкоженных публичных хоста
    /// GitHub — гейт для них не пробел, а реальная защита от будущей
    /// ошибки конфигурации, если константы когда-нибудь станут полем.
    gate_network: bool,
}

impl SelfUpdateDispatch {
    pub fn new(current_exe: PathBuf) -> Result<Self, String> {
        let client = Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|err| err.to_string())?;
        Ok(Self {
            client,
            current_exe,
            api_base: "https://api.github.com".to_string(),
            download_base: "https://github.com".to_string(),
            gate_network: true,
        })
    }

    #[cfg(test)]
    fn with_bases(current_exe: PathBuf, api_base: String, download_base: String) -> Self {
        let client = Client::builder().timeout(REQUEST_TIMEOUT).build().unwrap();
        Self {
            client,
            current_exe,
            api_base,
            download_base,
            gate_network: false,
        }
    }
}

impl ToolDispatch for SelfUpdateDispatch {
    fn call(&self, tool: &str, args: &Value) -> Result<Value, DispatchError> {
        let result: Result<Value, String> = match tool {
            "registry.get_latest" => (|| {
                if self.gate_network {
                    require_network(&self.api_base)?;
                }
                let channel = args
                    .get("channel")
                    .and_then(|v| v.as_str())
                    .unwrap_or("stable");
                let current_version = required_str(args, "current_version")?;
                registry_get_latest(&self.client, &self.api_base, channel, current_version)
            })(),
            "platform.resolve_asset" => (|| {
                let version = required_str(args, "version")?;
                resolve_asset(version)
            })(),
            "github.download_release_asset" => (|| {
                if self.gate_network {
                    require_network(&self.download_base)?;
                }
                let version = required_str(args, "version")?;
                let asset_name = required_str(args, "asset_name")?;
                let dest_dir = std::env::temp_dir()
                    .join(format!("berimor-self-update-{}", std::process::id()));
                download_release_asset(
                    &self.client,
                    &self.download_base,
                    version,
                    asset_name,
                    &dest_dir,
                )
            })(),
            "crypto.verify_signature" => (|| {
                let archive_path = required_str(args, "archive_path")?;
                Ok(verify_signature(Path::new(archive_path)))
            })(),
            "fs.atomic_replace_binary" => (|| {
                let archive_path = required_str(args, "archive_path")?;
                atomic_replace_binary(&self.current_exe, Path::new(archive_path))
            })(),
            "agent.self_check" => (|| {
                let expected_version = required_str(args, "expected_version")?;
                self_check(&self.current_exe, expected_version)
            })(),
            "fs.restore_from_checkpoint" => (|| {
                let backup_path = required_str(args, "backup_path")?;
                restore_from_checkpoint(&self.current_exe, Path::new(backup_path))
            })(),
            "registry.record_version" => Ok(registry_record_version(args)),
            "self_update.fail" => self_update_fail(args),
            "self_update.noop" => Ok(json!({})),
            other => Err(format!("неизвестный self-update-инструмент: {other}")),
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

/// Тот же паттерн, что `http_provider.rs::check_network_gate` — self-update
/// обращается только к своим двум захардкоженным хостам GitHub (не к
/// конфигурируемому пользователем `base_url`, как у model-pool), поэтому
/// здесь нет `allow_private_endpoint`-обхода: приватная цель для этих
/// констант — не легитимный случай, а признак подмены. Редиректы
/// `github.com` → CDN blob-хранилище следуют штатным поведением `reqwest`
/// без дополнительного гейта на каждый хоп — редирект контролирует сам
/// GitHub, не атакуемый конфиг, тот же класс доверия, что `curl -L`.
fn require_network(base_url: &str) -> Result<(), String> {
    match net_gate::check_host(host_of(base_url), 443) {
        NetworkDecision::Allow => Ok(()),
        NetworkDecision::ConfirmRequired { reason } => Err(format!(
            "сетевой гейт: {reason} (self-update обращается только к github.com/api.github.com)"
        )),
    }
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
        .header("User-Agent", "berimor-self-update")
        .send()
        .map_err(|err| err.to_string())?;
    if !response.status().is_success() {
        return Err(format!("GitHub API вернул {}", response.status()));
    }
    response
        .json::<GitHubRelease>()
        .map_err(|err| err.to_string())
}

/// Независимое ревью (MAJOR): пока проект в `0.x` (сегодня `0.6.0`,
/// `Cargo.toml`), `latest.major > current.major` не сработает НИКОГДА —
/// `major` равен `0` для любого перехода `0.x → 0.y`, `major_gate` в графе
/// молчал бы до `1.0.0` независимо от размера скачка. SemVer §4 (initial
/// development): в `0.x` именно minor-бамп — сигнал несовместимых
/// изменений, аналог major в `≥1.0`.
fn compute_version_flags(current: &str, latest: &str) -> Result<(bool, bool), String> {
    let current = semver::Version::parse(current.trim_start_matches('v'))
        .map_err(|err| format!("текущая версия '{current}' не semver: {err}"))?;
    let latest = semver::Version::parse(latest.trim_start_matches('v'))
        .map_err(|err| format!("версия релиза '{latest}' не semver: {err}"))?;
    let is_newer = latest > current;
    let is_major_bump = is_newer
        && (latest.major > current.major || (current.major == 0 && latest.minor > current.minor));
    Ok((is_newer, is_major_bump))
}

/// `channel`/`current_version` — свободная функция (не метод), тестируема
/// на любом `api_base` без прохода через `require_network`/`call()`.
fn registry_get_latest(
    client: &Client,
    api_base: &str,
    channel: &str,
    current_version: &str,
) -> Result<Value, String> {
    if channel != "stable" {
        return Err(format!(
            "канал '{channel}' ещё не поддержан (только stable — ROADMAP D4, нет схемы тегирования beta/canary в release.yml)"
        ));
    }
    let release = fetch_latest_release(client, api_base, RELEASE_REPOSITORY)?;
    let version = release.tag_name.trim_start_matches('v').to_string();
    let (is_newer, is_major_bump) = compute_version_flags(current_version, &version)?;
    Ok(json!({
        "version": version,
        "is_newer": is_newer,
        "is_major_bump": is_major_bump,
    }))
}

/// Зеркало `bootstrap/src/platform.ts::detectPlatform` — то же соглашение
/// имени артефакта, `std::env::consts::OS`/`ARCH` требуют явного маппинга
/// (Rust отдаёт `"macos"`/`"windows"`, не `"darwin"`/`"win32"`).
fn resolve_asset(version: &str) -> Result<Value, String> {
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
        "asset_name": format!("berimor-{version}-{platform}-{arch}.{ext}"),
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

/// Качает архив И его `.sigstore.json`-бандл рядом (то же соглашение
/// имени, что `verify.rs::bundle_path_for` — D2) в один временный каталог.
fn download_release_asset(
    client: &Client,
    base_url: &str,
    version: &str,
    asset_name: &str,
    dest_dir: &Path,
) -> Result<Value, String> {
    std::fs::create_dir_all(dest_dir).map_err(|err| err.to_string())?;

    let archive_path = dest_dir.join(asset_name);
    let archive_url =
        format!("{base_url}/{RELEASE_REPOSITORY}/releases/download/v{version}/{asset_name}");
    download_file(client, &archive_url, &archive_path)?;

    let sidecar_name = format!("{asset_name}.sigstore.json");
    let sidecar_path = dest_dir.join(&sidecar_name);
    let sidecar_url =
        format!("{base_url}/{RELEASE_REPOSITORY}/releases/download/v{version}/{sidecar_name}");
    download_file(client, &sidecar_url, &sidecar_path)?;

    Ok(json!({"archive_path": archive_path.display().to_string()}))
}

fn verify_signature(archive_path: &Path) -> Value {
    match crate::verify::verify_artifact(archive_path) {
        Ok(()) => json!({"ok": true}),
        Err(err) => json!({"ok": false, "error": err.to_string()}),
    }
}

fn registry_record_version(args: &Value) -> Value {
    // Сам факт, что этот Tool-шаг выполнился, уже журналируется движком
    // как `StepApplied` (M7-стиль аудит-след) — отдельное постоянное
    // хранилище «текущей версии» не нужно: следующий запуск self-update
    // всегда знает свою версию заново из `env!("CARGO_PKG_VERSION")`.
    let version = args.get("version").cloned().unwrap_or(Value::Null);
    let recorded_at_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    json!({"version": version, "recorded_at_unix": recorded_at_secs})
}

/// Всегда `Err` — обрывает процесс (не преодолевается подтверждением, I6:
/// это структурное свойство графа, на пути к `self_update.fail` нет
/// `human_gate`, не отдельная проверка флага здесь).
fn self_update_fail(args: &Value) -> Result<Value, String> {
    let reason = args
        .get("reason")
        .and_then(|v| v.as_str())
        .unwrap_or("самообновление остановлено");
    Err(reason.to_string())
}

/// Тот же паттерн, что `bootstrap/src/extract.ts` (D3): нативные средства
/// платформы, не новая crate-зависимость — unix `tar`, Windows
/// `Expand-Archive` через PowerShell.
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

/// Ищет осиротевший бэкап рядом с `current_exe` по префиксу имени
/// (`<имя>.backup-*`) — см. doc-комментарий `atomic_replace_binary`
/// (независимое ревью, CRITICAL-1) про окно между двумя `rename`.
fn find_orphaned_backup(current_exe: &Path) -> Option<PathBuf> {
    let parent = current_exe.parent()?;
    let file_name = current_exe.file_name()?.to_str()?;
    let prefix = format!("{file_name}.backup-");
    std::fs::read_dir(parent)
        .ok()?
        .filter_map(|entry| entry.ok())
        .find_map(|entry| {
            let name = entry.file_name();
            name.to_str()
                .filter(|n| n.starts_with(&prefix))
                .map(|_| entry.path())
        })
}

/// Распаковывает `archive_path` и подставляет новый бинарник на место
/// `current_exe`. Бэкап и подготовка отката — одним действием:
/// переименование ТЕКУЩЕГО файла в сторону (работает даже для сейчас
/// исполняемого файла на unix; на Windows переименование запущенного
/// `.exe` разрешено ОС, перезапись на месте — нет, отсюда именно
/// rename-стратегия, не copy-over). Если вторая перестановка не удалась —
/// откатывает бэкап обратно, не оставляя систему без исполняемого файла
/// на месте вообще.
///
/// **Независимое ревью (CRITICAL-1), честно принятый остаточный пробел:**
/// с точки зрения Process Engine шаг `swap` — один неделимый Tool-вызов
/// (журнал узнаёт о нём только ПОСЛЕ успешного возврата `dispatch.call()`,
/// `engine.rs::execute_single_step`). Если процесс/машина падает МЕЖДУ
/// двумя `rename` ниже (после первого, до второго) — журнал не содержит
/// об этом вообще ничего, `--resume` не может использовать `backup_path`
/// (он не был записан в состояние). Полностью резюмируемым это окно не
/// сделать без персистентного журнала вне Process Engine — вне рамок этой
/// задачи. Минимальный фикс здесь — не оставлять оператора с непонятной
/// ошибкой ОС: если `current_exe` уже отсутствует к началу вызова (типичный
/// след прерванной попытки), явно ищем осиротевший бэкап и называем его.
fn atomic_replace_binary(current_exe: &Path, archive_path: &Path) -> Result<Value, String> {
    if !current_exe.exists() {
        return Err(match find_orphaned_backup(current_exe) {
            Some(backup) => format!(
                "{} отсутствует — похоже, предыдущая попытка обновления прервалась между переименованиями; вероятный бэкап: {} (восстановите вручную: переименуйте его обратно в {})",
                current_exe.display(),
                backup.display(),
                current_exe.display()
            ),
            None => format!(
                "{} отсутствует, и резервная копия рядом не найдена — автоматическое восстановление невозможно",
                current_exe.display()
            ),
        });
    }

    let extract_dir = {
        let mut dir = archive_path.as_os_str().to_owned();
        dir.push(".extracted");
        PathBuf::from(dir)
    };
    std::fs::create_dir_all(&extract_dir).map_err(|err| err.to_string())?;
    extract_archive(archive_path, &extract_dir)?;

    let binary_name = if cfg!(windows) {
        "berimor.exe"
    } else {
        "berimor"
    };
    let new_binary = extract_dir.join(binary_name);
    if !new_binary.exists() {
        return Err(format!(
            "в распакованном архиве нет ожидаемого файла '{binary_name}'"
        ));
    }

    let backup_path = {
        let mut p = current_exe.as_os_str().to_owned();
        p.push(format!(".backup-{}", std::process::id()));
        PathBuf::from(p)
    };
    std::fs::rename(current_exe, &backup_path)
        .map_err(|err| format!("не удалось создать резервную копию: {err}"))?;
    if let Err(err) = std::fs::rename(&new_binary, current_exe) {
        let _ = std::fs::rename(&backup_path, current_exe);
        return Err(format!("не удалось подставить новый бинарник: {err}"));
    }
    Ok(json!({"backup_path": backup_path.display().to_string()}))
}

/// Пуре-функция оценки результата `<binary> --version` — вынесена из
/// `self_check`, чтобы тестироваться без реального запуска процесса
/// (см. тесты: `std::os::{unix,windows}::process::ExitStatusExt::from_raw`
/// фабрикует `ExitStatus` детерминированно, без фикстурных «бинарников»
/// на диске — тот класс риска, которого явно просил избежать пользователь).
fn evaluate_self_check(output: &std::process::Output, expected_version: &str) -> Value {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let ok = output.status.success() && stdout.contains(expected_version);
    json!({"ok": ok, "stdout": stdout.trim()})
}

/// Независимое ревью (CRITICAL-2): если новый бинарник настолько сломан,
/// что вообще не запускается (битый архив, потерянные права на exec,
/// неверная архитектура — самый вероятный в реальности класс поломки),
/// `Command::output()` возвращает `Err`, а не `Ok` с ненулевым кодом. Если
/// эту `Err` пробрасывать наружу как раньше, весь self-update ПРОЦЕСС
/// обрывается на шаге `smoke_test`, граф никогда не доходит до
/// `smoke_gate`/`rollback` — откат не срабатывает именно тогда, когда
/// нужнее всего. Поэтому неудача самого запуска — тоже `{"ok": false}`,
/// не `Err`: пусть граф решает через `smoke_gate`, как и было задумано.
fn self_check(current_exe: &Path, expected_version: &str) -> Result<Value, String> {
    match std::process::Command::new(current_exe)
        .arg("--version")
        .output()
    {
        Ok(output) => Ok(evaluate_self_check(&output, expected_version)),
        Err(err) => Ok(json!({
            "ok": false,
            "error": format!("не удалось запустить {}: {err}", current_exe.display()),
        })),
    }
}

fn restore_from_checkpoint(current_exe: &Path, backup_path: &Path) -> Result<Value, String> {
    std::fs::rename(backup_path, current_exe)
        .map_err(|err| format!("не удалось восстановить резервную копию: {err}"))?;
    Ok(json!({"restored": true}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    /// Мини-HTTP-сервер на один или несколько запросов, без новой crate-
    /// зависимости на mock-фреймворк (тот же принцип минимальных
    /// зависимостей, что в D3) — каждому принятому соединению отвечает
    /// одним и тем же телом/статусом, этого достаточно для проверки
    /// клиентской логики (не поведения сервера).
    fn spawn_test_server(
        status_line: &'static str,
        body: Vec<u8>,
        requests: usize,
    ) -> (String, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = std::thread::spawn(move || {
            for _ in 0..requests {
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

    #[test]
    fn registry_get_latest_reports_newer_and_major_bump() {
        let (base, handle) = spawn_test_server("200 OK", br#"{"tag_name": "v2.0.0"}"#.to_vec(), 1);
        let client = Client::builder().timeout(REQUEST_TIMEOUT).build().unwrap();
        let result = registry_get_latest(&client, &base, "stable", "1.5.0").unwrap();
        assert_eq!(result["version"], "2.0.0");
        assert_eq!(result["is_newer"], true);
        assert_eq!(result["is_major_bump"], true);
        handle.join().unwrap();
    }

    #[test]
    fn registry_get_latest_reports_not_newer_when_same_version() {
        let (base, handle) = spawn_test_server("200 OK", br#"{"tag_name": "v1.5.0"}"#.to_vec(), 1);
        let client = Client::builder().timeout(REQUEST_TIMEOUT).build().unwrap();
        let result = registry_get_latest(&client, &base, "stable", "1.5.0").unwrap();
        assert_eq!(result["is_newer"], false);
        assert_eq!(result["is_major_bump"], false);
        handle.join().unwrap();
    }

    #[test]
    fn registry_get_latest_minor_bump_is_not_major() {
        let (base, handle) = spawn_test_server("200 OK", br#"{"tag_name": "v1.6.0"}"#.to_vec(), 1);
        let client = Client::builder().timeout(REQUEST_TIMEOUT).build().unwrap();
        let result = registry_get_latest(&client, &base, "stable", "1.5.0").unwrap();
        assert_eq!(result["is_newer"], true);
        assert_eq!(result["is_major_bump"], false);
        handle.join().unwrap();
    }

    /// Независимое ревью (MAJOR): в `0.x` (проект сегодня на `0.6.0`)
    /// minor-бамп обязан считаться major-подобным — иначе `major_gate`
    /// не сработает вообще ни разу до `1.0.0`.
    #[test]
    fn registry_get_latest_minor_bump_in_0x_counts_as_major() {
        let (base, handle) = spawn_test_server("200 OK", br#"{"tag_name": "v0.7.0"}"#.to_vec(), 1);
        let client = Client::builder().timeout(REQUEST_TIMEOUT).build().unwrap();
        let result = registry_get_latest(&client, &base, "stable", "0.6.0").unwrap();
        assert_eq!(result["is_newer"], true);
        assert_eq!(result["is_major_bump"], true);
        handle.join().unwrap();
    }

    #[test]
    fn registry_get_latest_patch_bump_in_0x_is_not_major() {
        let (base, handle) = spawn_test_server("200 OK", br#"{"tag_name": "v0.6.1"}"#.to_vec(), 1);
        let client = Client::builder().timeout(REQUEST_TIMEOUT).build().unwrap();
        let result = registry_get_latest(&client, &base, "stable", "0.6.0").unwrap();
        assert_eq!(result["is_newer"], true);
        assert_eq!(result["is_major_bump"], false);
        handle.join().unwrap();
    }

    #[test]
    fn registry_get_latest_rejects_unsupported_channel_explicitly() {
        let client = Client::builder().timeout(REQUEST_TIMEOUT).build().unwrap();
        let err = registry_get_latest(&client, "http://127.0.0.1:1", "beta", "1.0.0").unwrap_err();
        assert!(err.contains("ещё не поддержан"));
    }

    #[test]
    fn resolve_asset_names_current_platform() {
        let result = resolve_asset("1.2.3").unwrap();
        let name = result["asset_name"].as_str().unwrap();
        assert!(name.starts_with("berimor-1.2.3-"));
        assert!(name.ends_with(".tar.gz") || name.ends_with(".zip"));
    }

    #[test]
    fn download_release_asset_fetches_archive_and_sidecar() {
        let (base, handle) = spawn_test_server("200 OK", b"archive-bytes".to_vec(), 2);
        let client = Client::builder().timeout(REQUEST_TIMEOUT).build().unwrap();
        let dest_dir =
            std::env::temp_dir().join(format!("berimor-self-update-test-{}", uuid_like_suffix()));
        let result = download_release_asset(
            &client,
            &base,
            "1.2.3",
            "berimor-1.2.3-linux-x64.tar.gz",
            &dest_dir,
        )
        .unwrap();
        let archive_path = result["archive_path"].as_str().unwrap();
        assert_eq!(std::fs::read(archive_path).unwrap(), b"archive-bytes");
        assert!(dest_dir
            .join("berimor-1.2.3-linux-x64.tar.gz.sigstore.json")
            .exists());
        handle.join().unwrap();
        std::fs::remove_dir_all(&dest_dir).ok();
    }

    #[test]
    fn download_release_asset_propagates_http_error() {
        let (base, handle) = spawn_test_server("404 Not Found", Vec::new(), 1);
        let client = Client::builder().timeout(REQUEST_TIMEOUT).build().unwrap();
        let dest_dir =
            std::env::temp_dir().join(format!("berimor-self-update-test-{}", uuid_like_suffix()));
        let result = download_release_asset(&client, &base, "1.2.3", "asset.tar.gz", &dest_dir);
        assert!(result.is_err());
        // Первый запрос (архив) уже вернул 404 — второй (sidecar) сервер не
        // успевает принять, поток сервера завершится по drop listener'а.
        drop(handle);
        std::fs::remove_dir_all(&dest_dir).ok();
    }

    #[test]
    fn registry_record_version_echoes_version_with_timestamp() {
        let result = registry_record_version(&json!({"version": "1.2.3"}));
        assert_eq!(result["version"], "1.2.3");
        assert!(result["recorded_at_unix"].as_u64().unwrap() > 0);
    }

    #[test]
    fn self_update_fail_is_always_an_error() {
        let result = self_update_fail(&json!({"reason": "verify failed"}));
        assert_eq!(result.unwrap_err(), "verify failed");
    }

    #[test]
    fn self_update_fail_has_a_default_reason() {
        let result = self_update_fail(&json!({}));
        assert!(result.is_err());
    }

    /// Уникальный суффикс для временных путей тестов, идущих параллельно в
    /// разных потоках одного процесса. Только `SystemTime::now()` не
    /// гарантирует уникальности — на macOS-раннере CI два вызова из разных
    /// потоков, попавшие в одно и то же деление часов (более грубое
    /// разрешение, чем на Linux), получали ОДИНАКОВЫЙ путь, и один тест
    /// удалял каталог, пока другой ещё писал в него ("No such file or
    /// directory" на `fs::write` — найдено на реальном прогоне CI, Rust ·
    /// macos-latest, не в теории). Атомарный счётчик даёт уникальность
    /// независимо от разрешения часов.
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
            "berimor-self-update-{label}-{}",
            uuid_like_suffix()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Собирает архив нативным инструментом платформы — симметрично
    /// `extract_archive`, без committed бинарной фикстуры: тест сам
    /// упаковывает и распаковывает, проверяя оба направления сразу.
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
        // Глоб `\*` обязан быть ВНУТРИ кавычек одним токеном — снаружи
        // PowerShell разбирает `'...'\*` как два позиционных аргумента и
        // падает на "positional parameter cannot be found" (найдено на
        // реальном прогоне CI, Rust · windows-latest, не в теории).
        let escaped = |s: &str| format!("'{}'", s.replace('\'', "''"));
        let glob = format!("{}\\*", dir_to_pack.display());
        let status = std::process::Command::new("powershell")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                &format!(
                    "Compress-Archive -Path {} -DestinationPath {} -Force",
                    escaped(&glob),
                    escaped(&archive_path.display().to_string())
                ),
            ])
            .status()
            .unwrap();
        assert!(status.success());
    }

    fn binary_name() -> &'static str {
        if cfg!(windows) {
            "berimor.exe"
        } else {
            "berimor"
        }
    }

    fn test_archive_extension() -> &'static str {
        if cfg!(windows) {
            "zip"
        } else {
            "tar.gz"
        }
    }

    #[test]
    fn atomic_replace_binary_backs_up_old_and_swaps_in_new() {
        let work = temp_work_dir("swap");
        let pack_dir = work.join("pack");
        std::fs::create_dir_all(&pack_dir).unwrap();
        std::fs::write(pack_dir.join(binary_name()), b"new-binary-content").unwrap();

        let archive_path = work.join(format!("update.{}", test_archive_extension()));
        pack_test_archive(&pack_dir, &archive_path);

        let current_exe = work.join(binary_name());
        std::fs::write(&current_exe, b"old-binary-content").unwrap();

        let result = atomic_replace_binary(&current_exe, &archive_path).unwrap();
        let backup_path = PathBuf::from(result["backup_path"].as_str().unwrap());

        assert_eq!(std::fs::read(&current_exe).unwrap(), b"new-binary-content");
        assert_eq!(std::fs::read(&backup_path).unwrap(), b"old-binary-content");

        std::fs::remove_dir_all(&work).ok();
    }

    #[test]
    fn atomic_replace_binary_leaves_current_exe_untouched_if_archive_lacks_expected_name() {
        let work = temp_work_dir("missing-binary");
        let pack_dir = work.join("pack");
        std::fs::create_dir_all(&pack_dir).unwrap();
        std::fs::write(pack_dir.join("not-the-right-name"), b"x").unwrap();

        let archive_path = work.join(format!("update.{}", test_archive_extension()));
        pack_test_archive(&pack_dir, &archive_path);

        let current_exe = work.join(binary_name());
        std::fs::write(&current_exe, b"old-binary-content").unwrap();

        let result = atomic_replace_binary(&current_exe, &archive_path);
        assert!(result.is_err());
        assert_eq!(std::fs::read(&current_exe).unwrap(), b"old-binary-content");

        std::fs::remove_dir_all(&work).ok();
    }

    #[test]
    fn restore_from_checkpoint_renames_backup_back_into_place() {
        let work = temp_work_dir("rollback");
        let current_exe = work.join(binary_name());
        let backup_path = work.join(format!("{}.backup-1", binary_name()));
        std::fs::write(&current_exe, b"broken-new-binary").unwrap();
        std::fs::write(&backup_path, b"old-good-binary").unwrap();

        let result = restore_from_checkpoint(&current_exe, &backup_path).unwrap();
        assert_eq!(result["restored"], true);
        assert_eq!(std::fs::read(&current_exe).unwrap(), b"old-good-binary");
        assert!(!backup_path.exists());

        std::fs::remove_dir_all(&work).ok();
    }

    #[cfg(unix)]
    fn fake_output(exit_code: i32, stdout: &str) -> std::process::Output {
        use std::os::unix::process::ExitStatusExt;
        std::process::Output {
            status: std::process::ExitStatus::from_raw(exit_code << 8),
            stdout: stdout.as_bytes().to_vec(),
            stderr: Vec::new(),
        }
    }

    #[cfg(windows)]
    fn fake_output(exit_code: i32, stdout: &str) -> std::process::Output {
        use std::os::windows::process::ExitStatusExt;
        std::process::Output {
            status: std::process::ExitStatus::from_raw(exit_code as u32),
            stdout: stdout.as_bytes().to_vec(),
            stderr: Vec::new(),
        }
    }

    #[test]
    fn evaluate_self_check_ok_when_exit_zero_and_version_matches() {
        let output = fake_output(0, "berimor 1.2.3\n");
        let result = evaluate_self_check(&output, "1.2.3");
        assert_eq!(result["ok"], true);
    }

    #[test]
    fn evaluate_self_check_not_ok_when_version_mismatches() {
        let output = fake_output(0, "berimor 9.9.9\n");
        let result = evaluate_self_check(&output, "1.2.3");
        assert_eq!(result["ok"], false);
    }

    #[test]
    fn evaluate_self_check_not_ok_when_exit_nonzero_even_if_stdout_matches() {
        let output = fake_output(1, "berimor 1.2.3\n");
        let result = evaluate_self_check(&output, "1.2.3");
        assert_eq!(result["ok"], false);
    }

    /// Независимое ревью (CRITICAL-2): бинарник, который вообще не
    /// запускается (не найден/не исполняемый), обязан давать `Ok({"ok":
    /// false})`, а не `Err` — иначе `smoke_gate`/`rollback` никогда не
    /// сработают для самого вероятного в реальности сценария поломки.
    #[test]
    fn self_check_returns_ok_false_not_err_when_binary_does_not_exist() {
        let missing =
            std::env::temp_dir().join(format!("berimor-self-check-missing-{}", uuid_like_suffix()));
        let result = self_check(&missing, "1.2.3").unwrap();
        assert_eq!(result["ok"], false);
        assert!(result["error"]
            .as_str()
            .unwrap()
            .contains(&missing.display().to_string()));
    }

    /// Независимое ревью (CRITICAL-1): повторная попытка `swap` после
    /// того, как предыдущая упала между двумя `rename` (`current_exe`
    /// уже отсутствует, бэкап рядом остался) — сообщение обязано называть
    /// найденный бэкап, не быть непонятной ошибкой ОС.
    #[test]
    fn atomic_replace_binary_names_orphaned_backup_on_retry() {
        let work = temp_work_dir("orphaned-backup");
        let current_exe = work.join(binary_name());
        let backup_path = work.join(format!("{}.backup-12345", binary_name()));
        std::fs::write(&backup_path, b"old-good-binary").unwrap();
        // current_exe намеренно НЕ создаём — имитирует прерванную попытку.

        let fake_archive = work.join(format!("update.{}", test_archive_extension()));
        let result = atomic_replace_binary(&current_exe, &fake_archive);
        let err = result.unwrap_err();
        assert!(
            err.contains(&backup_path.display().to_string()),
            "сообщение обязано называть найденный бэкап: {err}"
        );

        std::fs::remove_dir_all(&work).ok();
    }

    /// Сервер, отвечающий N подключениям по очереди разными телами — для
    /// сценариев, где один прогон делает несколько разных HTTP-запросов
    /// подряд (get_latest → download архива → download sidecar).
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

    /// Доказывает, что подтверждение не запрашивалось вовсе — режим `Off`
    /// плюс `mutates: false` политика читающих self-update-инструментов
    /// (используется только `registry.get_latest` в обоих golden-тестах,
    /// мутирующие шаги до них не доходят).
    struct PanicIfAsked;
    impl ConfirmationHandler for PanicIfAsked {
        fn confirm(
            &self,
            _action: &berimor_types::capability::ProposedAction,
            _reason: &str,
        ) -> bool {
            panic!("self-update golden-тест не должен запрашивать подтверждение")
        }
    }

    /// Контрактный тест на `fixtures/golden/processes/agent-self-update.yaml`
    /// целиком — путь «обновление не нужно»: единственный сетевой вызов —
    /// `registry.get_latest`, `needs_update` ведёт прямо на `done`
    /// (ROADMAP D4 — граф спроектирован так, что это единственная
    /// физическая точка `Finished`, см. doc-комментарий модуля).
    #[test]
    fn golden_process_no_update_available_finishes_at_done() {
        let (base, handle) =
            spawn_sequenced_server(vec![("200 OK", br#"{"tag_name": "v0.6.0"}"#.to_vec())]);

        let process = parser::parse(PROCESS_YAML).unwrap();
        let storage = SqliteEventLog::open_in_memory().unwrap();
        let input = json!({"local": {"version": "0.6.0", "channel": "stable"}});
        let id = ProcessInstanceId("golden-no-update".to_string());
        let mut instance = engine::instantiate(&storage, id, process, input).unwrap();

        let current_exe = std::env::temp_dir().join("berimor-golden-test-fake-exe-noop");
        let dispatch = SelfUpdateDispatch::with_bases(current_exe, base.clone(), base);
        let gate = StandardCapability::new(std::env::temp_dir(), self_update_tool_policies());
        let confirmer = PanicIfAsked;
        let executor = SelfUpdateExecutor {
            gate: &gate,
            mode: ConfirmationMode::Off,
            confirmer: &confirmer,
            dispatch: &dispatch,
        };

        let outcome = engine::run(&storage, &executor, &mut instance).unwrap();
        assert_eq!(outcome, engine::RunOutcome::Finished);
        assert_eq!(instance.state["check_version"]["is_newer"], false);

        handle.join().unwrap();
    }

    /// Тот же граф — путь неудачной верификации: `download` реально качает
    /// (фейковые) байты, `verify` не проходит (не JSON — `verify_artifact`
    /// падает на разборе бандла ДО сетевого обращения к доверенному корню
    /// sigstore, тест не зависит от реальной сети), `verify_gate` ведёт на
    /// `fail_update`, который ВСЕГДА `Err` — процесс обрывается, не
    /// `Finished` (I6: ошибка верификации не преодолевается подтверждением
    /// — здесь это структурное свойство графа: на этом пути нет human_gate
    /// вообще).
    #[test]
    fn golden_process_verify_failure_routes_to_fail_update_not_finished() {
        // v0.6.1, не v0.7.0: с 0.x-осознанной семантикой is_major_bump
        // (независимое ревью, MAJOR) минорный бамп внутри 0.x сам по себе
        // считается major-подобным и увёл бы этот тест на human_review —
        // здесь проверяется путь verify_gate, не major_gate, поэтому нужен
        // именно patch-бамп.
        let (base, handle) = spawn_sequenced_server(vec![
            ("200 OK", br#"{"tag_name": "v0.6.1"}"#.to_vec()),
            ("200 OK", b"fake-archive-bytes".to_vec()),
            ("200 OK", b"not a real sigstore bundle".to_vec()),
        ]);

        let process = parser::parse(PROCESS_YAML).unwrap();
        let storage = SqliteEventLog::open_in_memory().unwrap();
        let input = json!({"local": {"version": "0.6.0", "channel": "stable"}});
        let id = ProcessInstanceId("golden-verify-failure".to_string());
        let mut instance = engine::instantiate(&storage, id, process, input).unwrap();

        let current_exe = std::env::temp_dir().join("berimor-golden-test-fake-exe-verify-fail");
        let dispatch = SelfUpdateDispatch::with_bases(current_exe, base.clone(), base);
        let gate = StandardCapability::new(std::env::temp_dir(), self_update_tool_policies());
        let confirmer = PanicIfAsked;
        let executor = SelfUpdateExecutor {
            gate: &gate,
            mode: ConfirmationMode::Off,
            confirmer: &confirmer,
            dispatch: &dispatch,
        };

        let outcome = engine::run(&storage, &executor, &mut instance);
        assert!(
            outcome.is_err(),
            "verify_gate:false → fail_update обязан оборвать процесс, не Finished; получено {outcome:?}"
        );

        handle.join().unwrap();
    }
}
