//! Подкоманда `berimor run` — сборка Milestone 1 в реальный прогон.
//!
//! Источник: `docs/ROADMAP.md` §18.4 (CLI1–CLI3). Здесь впервые не тест
//! собирает цепочку: загрузка процесса (P1) → `SqliteEventLog` (F1) →
//! `instantiate`/`recover` (P3) → `run()` с настоящим `StepExecutor`,
//! маршрутизирующим `tool` → E1 через Capability (S1–S4) и
//! `llm_structured` → E2 (StructuredLLM → Model Pool E3 → провайдер E5 →
//! Mediation M1–M7). Остальные типы шагов — понятная ошибка «не
//! поддержано в Milestone 1», не молчаливый пропуск.

use crate::config::Config;
use crate::mcp_dispatch::{CompositeToolDispatch, McpToolDispatch};
use berimor_capability::confirm::{StandardCapability, ToolPolicy};
use berimor_context_engine::memory_builder::MemoryContextBuilder;
use berimor_executors::{
    structured_llm::StructuredLlm,
    tool_only::{self, ConfirmationHandler, StaticToolDispatch, ToolDispatch},
};
use berimor_model_pool::{
    http_provider::OpenAiCompatibleProvider, ModelEntry, ModelPool, ProviderKind,
};
use berimor_process_engine::{
    engine::{self, ExecutorError},
    parser,
};
use berimor_secrets::Secret;
use berimor_storage::{EventLog, SqliteEventLog};
use berimor_types::{
    capability::{ConfirmationMode, ProposedAction},
    event::{Event, EventKind, ProcessInstanceId},
    executor::ModelProvider,
    model::ModelIdentity,
    step::{Patch, Step, StepKind},
};
use serde_json::Value;
use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum RunError {
    #[error("не удалось прочитать процесс {path}: {reason}")]
    ReadProcess { path: PathBuf, reason: String },
    #[error("не удалось разобрать процесс: {0}")]
    ParseProcess(String),
    #[error("не удалось открыть журнал {path}: {reason}")]
    OpenStorage { path: PathBuf, reason: String },
    #[error("некорректный --input (ожидается JSON-объект): {0}")]
    BadInput(String),
    #[error("движок: {0}")]
    Engine(#[from] engine::EngineError),
    #[error("выполнение остановлено на шаге human_gate: человек отклонил продолжение")]
    HumanDeclined,
    #[error("провайдер '{0}': переменная окружения с ключом задана, но пуста или не установлена")]
    MissingApiKey(String),
    #[error("не удалось подключить провайдера: {0}")]
    Provider(String),
    #[error("MCP-серверы инструментов: {0}")]
    Mcp(#[from] crate::mcp_dispatch::McpDispatchError),
}

/// Точка входа подкоманды `run`. Печатает в stdout: идентификатор
/// инстанса (для `--resume`), события прогона и финальное состояние
/// JSON — последнее и проверяет e2e-тест CLI4.
pub fn run(
    config: &Config,
    process_path: &str,
    resume: &Option<String>,
    input: &Option<String>,
) -> Result<(), RunError> {
    let process_text =
        std::fs::read_to_string(process_path).map_err(|err| RunError::ReadProcess {
            path: PathBuf::from(process_path),
            reason: err.to_string(),
        })?;
    let process =
        parser::parse(&process_text).map_err(|err| RunError::ParseProcess(err.to_string()))?;

    let storage =
        SqliteEventLog::open(&config.storage_path).map_err(|err| RunError::OpenStorage {
            path: config.storage_path.clone(),
            reason: err.to_string(),
        })?;

    // Инстанс: восстановление по журналу (CLI3) или новый (CLI1).
    let mut instance = match resume {
        Some(id) => {
            let id = ProcessInstanceId(id.clone());
            let recovered = engine::recover(&storage, process, id)?;
            println!(
                "[berimor] восстановлен инстанс {} (шаг: {:?})",
                recovered.id.0, recovered.current_step
            );
            recovered
        }
        None => {
            let input_json = match input {
                Some(text) => serde_json::from_str::<Value>(text)
                    .map_err(|err| RunError::BadInput(err.to_string()))?,
                None => Value::Object(serde_json::Map::new()),
            };
            let id = ProcessInstanceId(new_instance_id(&process_text));
            let instance = engine::instantiate(&storage, id, process, input_json)?;
            println!("[berimor] создан инстанс {}", instance.id.0);
            instance
        }
    };

    // --- Сборка исполнителей -------------------------------------------
    let workspace_root = std::env::current_dir()
        .and_then(|p| p.canonicalize())
        .unwrap_or_else(|_| PathBuf::from("."));

    let tool_policies: HashMap<String, ToolPolicy> = config
        .tool_stubs
        .iter()
        .map(|stub| {
            (
                stub.tool.clone(),
                ToolPolicy {
                    mutates: Some(stub.mutates),
                    ..Default::default()
                },
            )
        })
        .collect();
    let gate = StandardCapability::new(workspace_root, tool_policies);
    let static_stubs = StaticToolDispatch::new(
        config
            .tool_stubs
            .clone()
            .into_iter()
            .map(|s| (s.tool, s.response, s.mutates))
            .collect(),
    );
    // MCP-серверы (T1) — только если оператор явно перечислил их в
    // конфиге; пустой список ведёт себя побитово как раньше (только
    // static_stubs, ни одного сервера не запускается).
    let mcp = if config.mcp_servers.is_empty() {
        None
    } else {
        Some(McpToolDispatch::connect(&config.mcp_servers)?)
    };
    let dispatch = CompositeToolDispatch { mcp, static_stubs };
    let confirmer = TerminalConfirmer;

    let mut pool = ModelPool::new();
    let mut provider_clients: Vec<OpenAiCompatibleProvider> = Vec::new();
    for p in &config.providers {
        pool.register(ModelEntry {
            identity: ModelIdentity {
                provider: p.name.clone(),
                model_id: p.model_id.clone(),
                tier: p.tier,
            },
            kind: ProviderKind::Remote,
            cost_per_1k_tokens: p.cost_per_1k_tokens,
            measured_latency_ms: None,
        });
        let api_key = match &p.api_key_env {
            Some(env) => {
                let value = std::env::var(env).ok().filter(|v| !v.is_empty());
                Some(Secret::new(
                    value.ok_or_else(|| RunError::MissingApiKey(p.name.clone()))?,
                ))
            }
            None => None,
        };
        provider_clients.push(
            OpenAiCompatibleProvider::new(
                ModelIdentity {
                    provider: p.name.clone(),
                    model_id: p.model_id.clone(),
                    tier: p.tier,
                },
                p.base_url.clone(),
                api_key,
                p.allow_private_endpoint,
            )
            .map_err(|err| RunError::Provider(err.to_string()))?,
        );
    }
    let providers: HashMap<String, &dyn ModelProvider> = provider_clients
        .iter()
        .map(|c| (c.identity().provider.clone(), c as &dyn ModelProvider))
        .collect();

    // Телеметрия Mediation (M7) — события в тот же журнал; свёртка их
    // игнорирует, аудит-след их видит (security-model.md §5).
    let instance_id = instance.id.clone();
    let process_version = instance.process.version;
    let on_attempt = |kind: EventKind| {
        let _ = storage.append(Event::new(
            instance_id.clone(),
            process_version,
            kind,
            Value::Null,
        ));
    };

    let skills = load_skills(config.memory.skills_dir.as_deref());
    let memory_context = MemoryContextBuilder {
        episodic: &storage,
        skills: &skills,
        session_search_limit: config.memory.session_search_limit,
    };

    let llm = StructuredLlm {
        pool: &pool,
        providers: &providers,
        context: &memory_context,
        on_attempt: Some(&on_attempt),
    };

    let executor = CliExecutor {
        gate: &gate,
        mode: config.confirmation_mode,
        confirmer: &confirmer,
        dispatch: &dispatch,
        llm: &llm,
        latency_budget_ms: instance.process.limits.latency_budget_ms,
    };

    // --- Цикл прогона с обработкой human_gate (CLI2) --------------------
    loop {
        match engine::run(&storage, &executor, &mut instance)? {
            engine::RunOutcome::Finished => {
                println!("[berimor] процесс завершён");
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
                        "[berimor] остановлено на human_gate '{step_id}'; возобновить: berimor run {process_path} --resume {}",
                        instance.id.0
                    );
                    return Err(RunError::HumanDeclined);
                }
                let _ = storage.append(Event::new(
                    instance.id.clone(),
                    instance.process.version,
                    EventKind::HumanGateResolved,
                    Value::Null,
                ));
                // «Ответ возобновляет выполнение» (process-engine.md §5) —
                // повторный run с того же current_step.
            }
        }
    }
}

/// Реальный `StepExecutor` (CLI1): маршрутизация по типу шага.
struct CliExecutor<'a> {
    gate: &'a StandardCapability,
    mode: ConfirmationMode,
    confirmer: &'a TerminalConfirmer,
    dispatch: &'a dyn ToolDispatch,
    llm: &'a StructuredLlm<'a>,
    /// `ProcessLimits.latency_budget_ms` (P6, ADR-0011) — SLA отбора
    /// провайдера на КАЖДОМ `llm_structured`-шаге, не убывающий бюджет
    /// цикла: то же значение передаётся в каждый вызов `llm.execute`.
    latency_budget_ms: Option<u64>,
}

impl engine::StepExecutor for CliExecutor<'_> {
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
            StepKind::LlmStructured { contract, model_tier } => self
                .llm
                .execute(
                    &step.id,
                    contract,
                    *model_tier,
                    state,
                    self.latency_budget_ms,
                )
                .map_err(|err| ExecutorError {
                    step_id: step.id.clone(),
                    reason: err.to_string(),
                }),
            other => Err(ExecutorError {
                step_id: step.id.clone(),
                reason: format!(
                    "тип шага не поддержан в Milestone 1 (поддержаны: tool, llm_structured): {other:?}"
                ),
            }),
        }
    }
}

/// Подтверждение в терминале: «да» — opt-in, всё остальное (включая
/// EOF) — отказ.
struct TerminalConfirmer;

impl ConfirmationHandler for TerminalConfirmer {
    fn confirm(&self, action: &ProposedAction, reason: &str) -> bool {
        eprintln!("[berimor] capability: {reason}");
        eprintln!("[berimor] действие: {} {}", action.tool, action.args);
        ask_line("[berimor] подтвердить? [y/N] ")
    }
}

fn ask_human(step_id: &str, reason: &str) -> bool {
    eprintln!("[berimor] human_gate '{step_id}': {reason}");
    ask_line("[berimor] продолжить выполнение? [y/N] ")
}

fn ask_line(prompt: &str) -> bool {
    eprint!("{prompt}");
    let _ = std::io::stderr().flush();
    let mut answer = String::new();
    if std::io::stdin().read_line(&mut answer).is_err() {
        return false;
    }
    matches!(
        answer.trim().to_lowercase().as_str(),
        "y" | "yes" | "д" | "да"
    )
}

/// Интерполяция `{{state.path}}` внутри текста (human_gate `reason` —
/// шаблон с вкраплениями, в отличие от целостных плейсхолдеров аргументов
/// ToolOnly). Неразрешимый путь остаётся как есть — текст причины не
/// должен падать из-за шаблона.
fn interpolate(template: &str, state: &Value) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        match rest[start..].find("}}") {
            Some(end) => {
                let path = rest[start + 2..start + end].trim();
                match berimor_types::state_path::resolve(path, state) {
                    Some(value) => out.push_str(&match value {
                        Value::String(s) => s.clone(),
                        other => other.to_string(),
                    }),
                    None => out.push_str(&rest[start..start + end + 2]),
                }
                rest = &rest[start + end + 2..];
            }
            None => {
                out.push_str(&rest[start..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

/// Читает `skills_dir` и разбирает каждый файл через `procedural::parse_summary`
/// (только фронтматтер — тело навыку в контексте не нужно, §3
/// memory-model.md). Обогащение контекста не критично для прогона:
/// нечитаемая директория или неразбираемый файл — предупреждение в
/// stderr и пропуск, не фатальная ошибка `run`.
fn load_skills(
    skills_dir: Option<&std::path::Path>,
) -> Vec<berimor_memory::procedural::SkillSummary> {
    let Some(dir) = skills_dir else {
        return Vec::new();
    };
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) => {
            eprintln!(
                "[berimor] не удалось прочитать директорию навыков {}: {err}",
                dir.display()
            );
            return Vec::new();
        }
    };

    let mut skills = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let raw = match std::fs::read_to_string(&path) {
            Ok(raw) => raw,
            Err(err) => {
                eprintln!(
                    "[berimor] навык {}: не удалось прочитать: {err}",
                    path.display()
                );
                continue;
            }
        };
        match berimor_memory::procedural::parse_summary(&raw) {
            Ok(summary) => skills.push(summary),
            Err(err) => eprintln!("[berimor] навык {}: не разобран: {err}", path.display()),
        }
    }
    skills
}

fn new_instance_id(process_text: &str) -> String {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    // Имя процесса из текста не извлекаем — парсинг уже сделан выше, а
    // идентификатор обязан быть лишь уникальным и читаемым.
    let _ = process_text;
    format!("run-{ms}-{}", std::process::id())
}
