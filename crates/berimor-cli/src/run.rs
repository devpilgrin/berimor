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
    agent_step::AgentStepExecutor,
    codeact::{CodeActExecutor, WasmHost},
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
    model::{ModelIdentity, ModelTierRequirement},
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
    #[error("не удалось создать jail рабочей области: {0}")]
    Jail(String),
    #[error("лиз инстанса: {0}")]
    InstanceLease(String),
    #[error(transparent)]
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
    // Сборка исполнителей — ДО instantiate: реестр секретов (S5) нужен
    // для маскировки входа на границе CLI (находка 1 независимого ревью).
    let bundle = build_executor_bundle(config)?;
    let providers = bundle.providers();

    // Находка 3.14 аудита: --resume с --input молча игнорировал вход —
    // пользователь полагал, что данные дошли. Честная ошибка.
    if resume.is_some() && input.is_some() {
        return Err(RunError::BadInput(
            "--input не применяется при --resume (вход инстанса уже зафиксирован в журнале) — уберите один из флагов".to_string(),
        ));
    }
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
            // S5: вход маскируется ДО instantiate — иначе значение
            // зарегистрированного секрета из --input ушло бы открытым
            // текстом в журнал (payload Instantiated + FTS-индекс), в
            // task_state-слой контекста модели (нарушение I4) и в stdout
            // финального состояния. Журнал «наследует» маскировку только
            // при замаскированном входе; место секретов — реестр
            // (secret_envs, ключи провайдеров), не состояние процесса.
            let input_json = bundle.masker.mask_value(&input_json);
            let id = ProcessInstanceId(new_instance_id(&process_text));
            let instance = engine::instantiate(&storage, id, process, input_json)?;
            println!("[berimor] создан инстанс {}", instance.id.0);
            instance
        }
    };

    // Межпроцессный лиз «один писатель на инстанс» (аудит 1.11, P5):
    // два CLI-процесса, восстановивших один инстанс из общего журнала,
    // больше не продвигают его параллельно — второму отказ. Лок-файлы
    // рядом с журналом; flock снимается ядром при смерти процесса.
    let locks_dir = std::path::PathBuf::from(format!("{}.locks", config.storage_path.display()));
    let _instance_lease =
        berimor_process_engine::instance_lock::try_acquire_file_lease(&locks_dir, &instance.id)
            .map_err(|err| RunError::InstanceLease(err.to_string()))?;

    // Телеметрия Mediation (M7) — события в тот же журнал; свёртка их
    // игнорирует, аудит-след их видит (security-model.md §5).
    let instance_id = instance.id.clone();
    let process_version = instance.process.version;
    let on_attempt = |kind: EventKind| {
        audit_append(
            &storage,
            Event::new(instance_id.clone(), process_version, kind, Value::Null),
        );
    };

    let memory_context = MemoryContextBuilder {
        episodic: &storage,
        skills: &bundle.skills,
        session_search_limit: config.memory.session_search_limit,
        entity_graph: config
            .memory
            .entity_graph
            .then_some(&storage as &dyn berimor_storage::EntityGraphStore),
        // HIGH ревью §20.5: контент слоя маскируется тем же реестром,
        // что вывод инструментов и подтверждения.
        masker: Some(bundle.masker.as_ref()),
    };

    let llm = StructuredLlm {
        pool: &bundle.pool,
        providers: &providers,
        context: &memory_context,
        on_attempt: Some(&on_attempt),
        secrets: bundle.masker.as_ref(),
    };

    let agent_step = AgentStepExecutor {
        pool: &bundle.pool,
        providers: &providers,
        context: &memory_context,
        on_attempt: Some(&on_attempt),
        gate: bundle.gate.as_ref(),
        mode: config.confirmation_mode,
        confirmer: bundle.confirmer.as_ref(),
        dispatch: bundle.dispatch.as_ref(),
        secrets: bundle.masker.as_ref(),
        on_tool_turn: None,
        on_provider_switch: None,
    };

    let wasm_host = WasmHost::new(
        bundle.dispatch.clone(),
        bundle.gate.clone(),
        config.confirmation_mode,
        bundle.confirmer.clone(),
        std::sync::Arc::clone(&bundle.masker),
    );
    let codeact = CodeActExecutor {
        pool: &bundle.pool,
        providers: &providers,
        context: &memory_context,
        on_attempt: Some(&on_attempt),
        wasm_host: &wasm_host,
        secrets: bundle.masker.as_ref(),
    };

    let executor = CliExecutor {
        gate: bundle.gate.as_ref(),
        mode: config.confirmation_mode,
        confirmer: bundle.confirmer.as_ref(),
        dispatch: bundle.dispatch.as_ref(),
        llm: &llm,
        agent_step: &agent_step,
        codeact: &codeact,
        latency_budget_ms: instance.process.limits.latency_budget_ms,
        masker: bundle.masker.as_ref(),
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
                // Записной путь памяти (memory-model.md §2/§4, opt-in):
                // извлечение фактов из финального состояния.
                if config.memory.fact_extraction {
                    extract_facts_with_similarity(
                        &llm,
                        &storage,
                        &instance,
                        bundle.masker.as_ref(),
                        config.memory.embeddings,
                    );
                }
                return Ok(());
            }
            engine::RunOutcome::AwaitingHuman { step_id, reason } => {
                let resolved_reason = bundle
                    .masker
                    .mask_text(&interpolate(&reason, &instance.state));
                audit_append(
                    &storage,
                    Event::new(
                        instance.id.clone(),
                        instance.process.version,
                        EventKind::HumanGateOpened {
                            reason: resolved_reason.clone(),
                        },
                        Value::Null,
                    ),
                );
                if !ask_human(&step_id, &resolved_reason) {
                    println!(
                        "[berimor] остановлено на human_gate '{step_id}'; возобновить: berimor run {process_path} --resume {}",
                        instance.id.0
                    );
                    return Err(RunError::HumanDeclined);
                }
                audit_append(
                    &storage,
                    Event::new(
                        instance.id.clone(),
                        instance.process.version,
                        EventKind::HumanGateResolved,
                        Value::Null,
                    ),
                );
                // «Ответ возобновляет выполнение» (process-engine.md §5) —
                // повторный run с того же current_step.
            }
        }
    }
}

/// Часть сборки исполнителей, не зависящая от конкретного инстанса
/// (gate/dispatch/pool/навыки) — общая для `run` (CLI-M1/M2) и `eval`
/// (CLI-M3, `observe.rs`). Что НЕ входит: `on_attempt`-телеметрия и
/// `latency_budget_ms` — оба привязаны к конкретному `ProcessInstance`
/// (id инстанса для телеметрии, `process.limits` для бюджета), собираются
/// вызывающим кодом после того, как инстанс/процесс уже известен.
///
/// `gate`/`dispatch`/`confirmer` — `Arc`, не голые значения: нужно
/// `CodeActExecutor`/`WasmHost` (E8) — `wasmtime::Linker::func_wrap`
/// требует `'static` на состояние `Store`, заимствование с временем
/// жизни этой функции не годится (тот же вынужденный выбор, что у
/// `HostState` в `codeact::wasm_host`, теперь распространяется на
/// точку сборки). Существующие потребители (`StructuredLlm`/
/// `AgentStepExecutor`/`CliExecutor`, которым достаточно `&dyn Trait`)
/// берут его через `Arc::as_ref`/автодеref — поведение не меняется.
pub(crate) struct ExecutorBundle {
    pub(crate) gate: std::sync::Arc<StandardCapability>,
    pub(crate) dispatch:
        std::sync::Arc<dyn berimor_executors::tool_only::ToolDispatch + Send + Sync>,
    pub(crate) pool: ModelPool,
    provider_clients: Vec<(String, std::sync::Arc<dyn ModelProvider + Send + Sync>)>,
    pub(crate) skills: Vec<berimor_memory::procedural::SkillSummary>,
    pub(crate) confirmer: std::sync::Arc<TerminalConfirmer>,
    /// Реестр секретов запуска (S5): ключи API провайдеров +
    /// `config.secret_envs`. Мост к хранилищу секретов (mediation.md §4.3)
    /// — значения покидают его только через `Secret::reveal` в заголовке
    /// HTTP-провайдера.
    pub(crate) masker: std::sync::Arc<berimor_secrets::Masker>,
}

impl ExecutorBundle {
    pub(crate) fn providers(&self) -> HashMap<String, &dyn ModelProvider> {
        self.provider_clients
            .iter()
            .map(|(name, client)| (name.clone(), client.as_ref() as &dyn ModelProvider))
            .collect()
    }
}

pub(crate) fn build_executor_bundle(config: &Config) -> Result<ExecutorBundle, RunError> {
    let workspace_root = std::env::current_dir()
        .and_then(|p| p.canonicalize())
        .unwrap_or_else(|_| PathBuf::from("."));

    let mut tool_policies: HashMap<String, ToolPolicy> = config
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
    // Политики встроенных инструментов (§20.10): декларируются кодом,
    // не конфигом — оператор не может объявить `terminal.exec`
    // неизменяющим. Имя встроенного инструмента зарезервировано.
    for (name, policy) in crate::builtin_dispatch::builtin_policies() {
        tool_policies.insert(name, policy);
    }
    // S2 (§20.3): основной путь `berimor run` — с jail-слоем. Домен
    // инструментов этого процесса — рабочая область пользователя (cwd);
    // first-party процессы с доменом шире cwd (self-update,
    // plugin-install) осознанно остаются на StandardCapability::new.
    let jail = berimor_capability::jail::FsJail::new(&workspace_root)
        .map_err(|err| RunError::Jail(err.to_string()))?;
    // Разрешения на мутации без вопроса (0.14.0): конфиг (глобальный +
    // проектный, union) + `.berimor-allow` в корне области. Deny-статика
    // и jail выше — разрешение снимает ВОПРОС, не запрет.
    let mut auto_confirm = config.auto_confirm.clone();
    for tool in crate::config::load_project_allow(&workspace_root) {
        if !auto_confirm.contains(&tool) {
            auto_confirm.push(tool);
        }
    }
    // Установленные плагины (§20.18): политики mutates из ACL-манифестов
    // — до построения гейта (политика точнее догадок вызывающего кода).
    let plugin_runtime = crate::plugin_runtime::PluginRuntimeDispatch::scan(
        &crate::plugin_install::plugins_root_dir(),
    );
    for (name, policy) in plugin_runtime.policies() {
        tool_policies.insert(name, policy);
    }
    let gate = StandardCapability::with_jail(jail, tool_policies).with_auto_confirm(auto_confirm);
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
    // Находка 3.15 аудита: MCP-инструмент молча затеняет статический
    // tool_stub с тем же именем (порядок: MCP до stubs) — предупреждение
    // оператору, не молчаливое переопределение поведения.
    if let Some(mcp_dispatch) = &mcp {
        for stub in &config.tool_stubs {
            if mcp_dispatch.has_tool(&stub.tool) {
                eprintln!(
                    "[berimor] ВНИМАНИЕ: MCP-инструмент '{}' затеняет статический tool_stub с тем же именем — будет вызван MCP",
                    stub.tool
                );
            }
        }
    }
    // Диспетчер: подписанные/доверенные артефакты — инструменты первого
    // класса (слой между встроенными и MCP).
    let dispatch = CompositeToolDispatch {
        builtin: crate::builtin_dispatch::BuiltinToolDispatch::new(workspace_root.clone()),
        plugin: (!plugin_runtime.is_empty()).then_some(plugin_runtime),
        mcp,
        static_stubs,
    };

    // Мост к хранилищу секретов (S5, точка 2 из mediation.md §4.3):
    // реестр запуска наполняется значениями из окружения — ключами
    // провайдеров и `secret_envs` — до сборки провайдеров, чтобы
    // зарегистрировать каждый ключ ровно один раз при чтении.
    let mut masker = berimor_secrets::Masker::new();
    // Находка 3 независимого ревью S5: значение короче MIN_SECRET_LEN
    // молча выпадает из реестра — оператор обязан это видеть, иначе
    // короткий ключ не защищён ничем, а владелец считает иначе.
    for name in &config.secret_envs {
        if let Ok(value) = std::env::var(name) {
            if !value.is_empty() && value.len() < berimor_secrets::MIN_SECRET_LEN {
                eprintln!(
                    "[berimor] предупреждение: значение {name} короче {} символов — не регистрируется в маскировщике (ни маскировки, ни контроля утечек)",
                    berimor_secrets::MIN_SECRET_LEN
                );
            }
        }
    }
    masker.register_from_env(&config.secret_envs);

    let mut pool = ModelPool::new();
    let mut provider_clients: Vec<(String, std::sync::Arc<dyn ModelProvider + Send + Sync>)> =
        Vec::new();
    // llama.cpp-бэкенд инициализируется один раз на процесс и только если
    // в конфигурации есть локальные провайдеры (E4, ADR-0024).
    #[cfg(feature = "local-inference")]
    let llama_backend: Option<
        std::sync::Arc<berimor_model_pool::local_provider::LlamaBackend>,
    > = if config.providers.iter().any(|p| p.model_path.is_some()) {
        Some(std::sync::Arc::new(
            berimor_model_pool::local_provider::LlamaBackend::init()
                .map_err(|err| RunError::Provider(format!("llama.cpp init: {err}")))?,
        ))
    } else {
        None
    };
    // Без feature-флага — заглушка того же формата для cfg-агностичного
    // вызова `build_local_provider` ниже; реальный бэкенд недоступен, и
    // `build_local_provider` в этой сборке всегда возвращает ошибку.
    #[cfg(not(feature = "local-inference"))]
    let llama_backend: Option<()> = None;
    for p in &config.providers {
        let identity = ModelIdentity {
            provider: p.name.clone(),
            model_id: p.model_id.clone(),
            tier: p.tier,
        };
        if let Some(model_path) = &p.model_path {
            // Локальный инференс (E4): kind=Local, нулевая предельная
            // стоимость — селектор ADR-0011 предпочтёт его при равном
            // классе; данные не покидают периметр (I5).
            pool.register(ModelEntry {
                identity: identity.clone(),
                kind: ProviderKind::Local,
                cost_per_1k_tokens: None,
                measured_latency_ms: None,
            });
            provider_clients.push((
                p.name.clone(),
                std::sync::Arc::from(build_local_provider(
                    &identity,
                    model_path,
                    llama_backend.as_ref(),
                )?),
            ));
            continue;
        }
        pool.register(ModelEntry {
            identity: identity.clone(),
            kind: ProviderKind::Remote,
            cost_per_1k_tokens: p.cost_per_1k_tokens,
            measured_latency_ms: None,
        });
        let api_key = match &p.api_key_env {
            Some(env) => {
                let value = std::env::var(env).ok().filter(|v| !v.is_empty());
                let key =
                    Secret::new(value.ok_or_else(|| RunError::MissingApiKey(p.name.clone()))?);
                if key.reveal().len() < berimor_secrets::MIN_SECRET_LEN {
                    eprintln!(
                        "[berimor] предупреждение: ключ провайдера '{}' короче {} символов — не регистрируется в маскировщике",
                        p.name,
                        berimor_secrets::MIN_SECRET_LEN
                    );
                }
                // Тот же ключ — известный секрет запуска: контроль утечек
                // (точка 4) обязан поймать его в выводе модели.
                masker.register(Secret::new(key.reveal().to_string()));
                Some(key)
            }
            None => None,
        };
        provider_clients.push((
            p.name.clone(),
            std::sync::Arc::new(
                OpenAiCompatibleProvider::new(
                    identity,
                    p.base_url.clone(),
                    api_key,
                    p.allow_private_endpoint,
                    p.temperature,
                )
                .map_err(|err| RunError::Provider(err.to_string()))?,
            ),
        ));
    }

    let skills = load_skills(config.memory.skills_dir.as_deref());
    let masker = std::sync::Arc::new(masker);
    let gate = std::sync::Arc::new(gate);
    let confirmer = std::sync::Arc::new(TerminalConfirmer {
        masker: std::sync::Arc::clone(&masker),
    });

    // agents.run (§20.17): обёртка над композитом — поручение субагенту
    // исполняется вложенным циклом со своими потолком/бюджетом/журналом.
    let dispatch: std::sync::Arc<dyn berimor_executors::tool_only::ToolDispatch + Send + Sync> =
        std::sync::Arc::new(crate::agent_dispatch::AgentRunDispatch::new(
            std::sync::Arc::new(dispatch),
            crate::agent_dispatch::AgentRunContext {
                pool: pool.clone(),
                providers: provider_clients.clone(),
                gate: gate.clone(),
                confirmer: confirmer.clone(),
                masker: masker.clone(),
                mode: config.confirmation_mode,
                storage_path: config.storage_path.clone(),
                builtin_names: crate::builtin_dispatch::builtin_policies()
                    .iter()
                    .map(|(n, _)| n.clone())
                    .collect(),
                spawn_stack: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            },
        ));

    Ok(ExecutorBundle {
        gate,
        dispatch,
        pool,
        provider_clients,
        skills,
        confirmer,
        masker,
    })
}

/// Конструктор локального провайдера — выделен, чтобы cfg-ветки feature
/// `local-inference` не расползались по циклу сборки. Бэкенд llama.cpp
/// общий на процесс — передаётся из `build_executor_bundle`.
#[cfg(feature = "local-inference")]
fn build_local_provider(
    identity: &ModelIdentity,
    model_path: &std::path::Path,
    backend: Option<&std::sync::Arc<berimor_model_pool::local_provider::LlamaBackend>>,
) -> Result<Box<dyn ModelProvider + Send + Sync>, RunError> {
    let backend = backend.cloned().ok_or_else(|| {
        RunError::Provider("внутренняя ошибка: llama.cpp backend не инициализирован".to_string())
    })?;
    let engine = berimor_model_pool::local_provider::LlamaCppEngine::load(backend, model_path)
        .map_err(|err| RunError::Provider(err.to_string()))?;
    Ok(Box::new(
        berimor_model_pool::local_provider::LlamaLocalProvider::new(identity.clone(), engine),
    ))
}

/// Без feature-флага локальный провайдер в конфигурации — жёсткая ошибка
/// с указанием способа включения, не молчаливый пропуск (fail-closed).
#[cfg(not(feature = "local-inference"))]
fn build_local_provider(
    identity: &ModelIdentity,
    model_path: &std::path::Path,
    _backend: Option<&()>,
) -> Result<Box<dyn ModelProvider + Send + Sync>, RunError> {
    Err(RunError::Provider(format!(
        "провайдер '{}': задан model_path ({}), но бинарник собран без локального инференса — пересоберите с `--features local-inference`",
        identity.provider,
        model_path.display()
    )))
}

/// Аудит-запись в журнал (находка 3.17 аудита): отказ append НЕ
/// проглатывается молча — предупреждение в stderr (аудит-след,
/// security-model §5: потеря события обязана быть видна).
pub(crate) fn audit_append(log: &dyn EventLog, event: Event) {
    if let Err(err) = log.append(event) {
        eprintln!("[berimor] ВНИМАНИЕ: событие аудит-журнала потеряно: {err}");
    }
}

/// Реальный `StepExecutor` (CLI1): маршрутизация по типу шага.
pub(crate) struct CliExecutor<'a> {
    pub(crate) gate: &'a StandardCapability,
    pub(crate) mode: ConfirmationMode,
    pub(crate) confirmer: &'a TerminalConfirmer,
    pub(crate) dispatch: &'a dyn ToolDispatch,
    pub(crate) llm: &'a StructuredLlm<'a>,
    pub(crate) agent_step: &'a AgentStepExecutor<'a>,
    pub(crate) codeact: &'a CodeActExecutor<'a>,
    /// Реестр секретов запуска (S5) — маскировка вывода tool-шагов.
    pub(crate) masker: &'a berimor_secrets::Masker,
    /// `ProcessLimits.latency_budget_ms` (P6, ADR-0011) — SLA отбора
    /// провайдера на КАЖДОМ `llm_structured`/`agent_step`/`codeact`-ходе,
    /// не убывающий бюджет цикла: то же значение передаётся в каждый
    /// вызов `llm.execute`/`agent_step.execute`/`codeact.execute`.
    pub(crate) latency_budget_ms: Option<u64>,
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
                self.masker,
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
            StepKind::AgentStep {
                contract,
                max_turns,
                self_critique,
                verify_actions,
            } => self
                .agent_step
                .execute(
                    &step.id,
                    contract,
                    *max_turns,
                    *self_critique,
                    *verify_actions,
                    state,
                    self.latency_budget_ms,
                )
                .map_err(|err| ExecutorError {
                    step_id: step.id.clone(),
                    reason: err.to_string(),
                }),
            StepKind::CodeAct {
                contract,
                tools,
                model_tier,
            } => self
                .codeact
                .execute(
                    &step.id,
                    contract,
                    tools,
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
                    "тип шага не поддержан в Milestone 1 (поддержаны: tool, llm_structured, agent_step, codeact): {other:?}"
                ),
            }),
        }
    }
}

/// Подтверждение в терминале: «да» — opt-in, всё остальное (включая
/// EOF) — отказ.
pub(crate) struct TerminalConfirmer {
    /// Третья точка маскировки (S5, mediation.md §4.3): текст
    /// подтверждения для человека не должен нести значения секретов.
    /// `action.args` уже замаскирован вызывающим (`tool_only`), `reason`
    /// (evidence deny-анализатора — фрагмент команды) маскируем здесь.
    pub(crate) masker: std::sync::Arc<berimor_secrets::Masker>,
}

impl ConfirmationHandler for TerminalConfirmer {
    fn confirm(&self, action: &ProposedAction, reason: &str) -> bool {
        eprintln!("[berimor] capability: {}", self.masker.mask_text(reason));
        eprintln!("[berimor] действие: {} {}", action.tool, action.args);
        ask_line("[berimor] подтвердить? [y/N] ")
    }
}

/// Записной путь памяти (memory-model.md §2/§4): конвейер «модель
/// предлагает факты → Mediation → дедупликация/конфликт → запись» для
/// финального состояния завершённого процесса. Один вызов модели на
/// процесс (не на шаг) — стоимость предсказуема; точка — после
/// Finished: извлечение из ФИНАЛЬНОГО состояния, а не из промежуточного
/// шума попыток.
///
/// Извлечение — фоновая операция: её сбой (модель недоступна, журнал
/// занят) НЕ хоронит уже завершённый процесс — предупреждение, не
/// отказ. Доверенная граница: кандидат проходит Mediation (тот же
/// контрактный путь, что у шагов — retry, policy, маскировка журнала),
/// конфликт с существующим фактом — событием человеку, не молчаливой
/// перезаписью (§2, I2).
/// Точка выбора источника близости для записного пути (ROADMAP §20.23):
/// `[memory] embeddings = true` И сборка с `--features embeddings` —
/// `VectorSimilarity` с реальным эмбеддером (fastembed,
/// multilingual-e5-small); иначе — `NoSimilarity` (дедупликация только по
/// точному хэшу, поведение до §20.23). Без скомпилированного feature
/// флаг конфигурации — no-op (бинарник пользователя не несёт ONNX
/// Runtime, притворяться, что эмбеддинги есть, нельзя).
#[cfg(not(feature = "embeddings"))]
fn extract_facts_with_similarity(
    llm: &StructuredLlm,
    storage: &SqliteEventLog,
    instance: &berimor_process_engine::ProcessInstance,
    masker: &berimor_secrets::Masker,
    use_embeddings: bool,
) {
    if use_embeddings {
        eprintln!(
            "[berimor] память: [memory] embeddings = true проигнорирован — бинарник собран без --features embeddings"
        );
    }
    extract_and_store_facts(
        llm,
        storage,
        instance,
        masker,
        &berimor_memory::semantic::NoSimilarity,
        None,
    );
}

/// См. cfg(not(embeddings))-вариант выше. Эмбеддер создаётся лениво
/// (конструктор не качает модель), поэтому построить его можно здесь, не
/// зная заранее, будут ли факты вовсе. Ошибка инференса внутри `embed_fn`
/// — пустой вектор: на записи он трактуется как «эмбеддинга нет» (факт
/// всё равно пишется), в `VectorSimilarity` — ошибкой размерности в
/// sqlite-vec наверх как `SimilarityError` (видимый сбой, не молчаливая
/// «непохожесть», находка 4.7 аудита).
#[cfg(feature = "embeddings")]
fn extract_facts_with_similarity(
    llm: &StructuredLlm,
    storage: &SqliteEventLog,
    instance: &berimor_process_engine::ProcessInstance,
    masker: &berimor_secrets::Masker,
    use_embeddings: bool,
) {
    use berimor_memory::semantic::{NoSimilarity, VectorSimilarity};

    if !use_embeddings {
        return extract_and_store_facts(llm, storage, instance, masker, &NoSimilarity, None);
    }
    let embedder = berimor_memory::embeddings::FastEmbedder::new();
    let embed_fn = |text: &str| embedder.embed(text).unwrap_or_default();
    let similarity = VectorSimilarity {
        store: storage,
        embed: &embed_fn,
    };
    extract_and_store_facts(llm, storage, instance, masker, &similarity, Some(&embed_fn));
}

/// Эмбеддер для записи вектора нового факта — шов той же формы, что у
/// `semantic::VectorSimilarity::embed`.
type EmbedFn<'a> = dyn Fn(&str) -> Vec<f32> + 'a;

/// Конвейер «модель предлагает факты → Mediation → дедупликация/конфликт
/// → запись» поверх уже выбранного источника близости (`similarity` —
/// `NoSimilarity` или `VectorSimilarity`, выбор делает
/// [`extract_facts_with_similarity`]). `embed_for_write` — тот же эмбеддер
/// для записи эмбеддинга нового факта в sqlite-vec (None — факт пишется
/// без вектора, `upsert_fact` не стирает уже сохранённый).
fn extract_and_store_facts(
    llm: &StructuredLlm,
    storage: &SqliteEventLog,
    instance: &berimor_process_engine::ProcessInstance,
    masker: &berimor_secrets::Masker,
    similarity: &dyn berimor_memory::semantic::SimilaritySource,
    embed_for_write: Option<&EmbedFn<'_>>,
) {
    use berimor_mediation::contracts::FactProposalBatch;
    use berimor_memory::semantic::{
        self, fact_hash, FactId, Resolution, StoredFact, DEFAULT_SIMILARITY_THRESHOLD,
    };
    use berimor_storage::{FactRecord, SemanticStore};
    use berimor_types::contract::Contract;

    let patch = match llm.execute(
        "extract_facts",
        FactProposalBatch::NAME,
        ModelTierRequirement::Weak,
        &instance.state,
        None,
    ) {
        Ok(patch) => patch,
        Err(err) => {
            eprintln!("[berimor] память: извлечение фактов пропущено ({err})");
            return;
        }
    };
    let batch: FactProposalBatch = match serde_json::from_value(patch.changes) {
        Ok(batch) => batch,
        Err(err) => {
            eprintln!("[berimor] память: не удалось разобрать пакет фактов ({err})");
            return;
        }
    };
    if batch.facts.is_empty() {
        return;
    }

    let existing_records = match storage.all_facts() {
        Ok(records) => records,
        Err(err) => {
            eprintln!("[berimor] память: не удалось прочитать факты ({err})");
            return;
        }
    };
    // StoredFact из записи хранилища: hash пересобирается из полей (те же
    // маскированные значения, что писались — хэш совпадёт с записанным).
    let to_stored = |record: &FactRecord| {
        StoredFact::rehydrate(
            FactId(record.id.clone()),
            record.subject.clone(),
            record.predicate.clone(),
            record.object.clone(),
            record.confidence,
            record.source.clone(),
            record.trusted_channel,
        )
    };
    let existing: Vec<StoredFact> = existing_records.iter().map(to_stored).collect();

    let mut written = 0usize;
    let mut merged = 0usize;
    let mut conflicts = 0usize;
    for proposal in &batch.facts {
        // 4.7 аудита: resolve возвращает Result — сбой источника близости
        // виден, а не «факты непохожи».
        let resolution = match semantic::resolve(
            proposal,
            &existing,
            similarity,
            DEFAULT_SIMILARITY_THRESHOLD,
        ) {
            Ok(resolution) => resolution,
            Err(err) => {
                eprintln!("[berimor] память: источник близости недоступен ({err}), факт пропущен");
                continue;
            }
        };
        match resolution {
            Resolution::Duplicate { .. } => {}
            Resolution::New => {
                // Id — от маскированных полей (как hash внутри
                // StoredFact::new): детерминирован, повторное извлечение
                // того же факта упирается в Duplicate.
                let masked_subject = masker.mask_text(&proposal.subject);
                let masked_predicate = masker.mask_text(&proposal.predicate);
                let masked_object = masker.mask_text(&proposal.object);
                let id = FactId(format!(
                    "f-{}",
                    fact_hash(&masked_subject, &masked_predicate, &masked_object).to_hex()
                ));
                let fact = StoredFact::new(id, proposal, false, masker);
                let record = FactRecord {
                    id: fact.id.0.clone(),
                    subject: fact.subject.clone(),
                    predicate: fact.predicate.clone(),
                    object: fact.object.clone(),
                    confidence: fact.confidence,
                    source: fact.source.clone(),
                    trusted_channel: fact.trusted_channel,
                };
                // §20.23: при включённых эмбеддингах новый факт пишется
                // сразу с вектором (тот же текст, что склеивает
                // VectorSimilarity) — иначе близкое совпадение по нему
                // невозможно до реиндексации. Пустой вектор (сбой
                // инференса) = «эмбеддинга нет», факт всё равно пишется.
                let embedding = embed_for_write
                    .map(|embed| {
                        embed(&format!(
                            "{} {} {}",
                            proposal.subject, proposal.predicate, proposal.object
                        ))
                    })
                    .filter(|v| !v.is_empty());
                if let Err(err) = storage.upsert_fact(&record, embedding.as_deref()) {
                    eprintln!("[berimor] память: не удалось записать факт ({err})");
                    continue;
                }
                written += 1;
            }
            Resolution::Merge { existing, .. } => {
                // Слияние — усиление уверенности СУЩЕСТВУЮЩЕГО факта, не
                // новая запись (§2: «слияние с существующим»).
                if let Some(mut record) = existing_records
                    .iter()
                    .find(|r| r.id == existing.0)
                    .cloned()
                {
                    record.confidence =
                        semantic::merge_confidence(record.confidence, proposal.confidence);
                    if let Err(err) = storage.upsert_fact(&record, None) {
                        eprintln!("[berimor] память: не удалось слить факт ({err})");
                        continue;
                    }
                    merged += 1;
                }
            }
            Resolution::Conflict(conflict) => {
                // «Не молчаливая перезапись»: конфликт — событием в
                // журнал + человеку в stderr. Извлечение идёт после
                // Finished — интерактивно спрашивать уже некогда.
                let detail = masker.mask_text(&format!(
                    "сохранённый факт {} («{}») против предложенного («{}»)",
                    conflict.existing.0, conflict.existing_object, conflict.candidate_object
                ));
                audit_append(
                    storage,
                    Event::new(
                        instance.id.clone(),
                        instance.process.version,
                        EventKind::MemoryConflict {
                            detail: detail.clone(),
                        },
                        Value::Null,
                    ),
                );
                eprintln!("[berimor] память: конфликт фактов (запись отклонена): {detail}");
                conflicts += 1;
            }
        }
    }
    if written + merged + conflicts > 0 {
        eprintln!("[berimor] память: записано {written}, слито {merged}, конфликтов {conflicts}");
    }
}

pub(crate) fn ask_human(step_id: &str, reason: &str) -> bool {
    eprintln!("[berimor] human_gate '{step_id}': {reason}");
    ask_line("[berimor] продолжить выполнение? [y/N] ")
}

pub(crate) fn ask_line(prompt: &str) -> bool {
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
pub(crate) fn interpolate(template: &str, state: &Value) -> String {
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
