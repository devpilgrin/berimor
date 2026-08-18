//! Инструмент `agents.run` (§20.17): поручение субагенту из agent.yaml.
//! Вложенный цикл AgentStep со СВОИМИ границами:
//!
//! - потолок инструментов = agent.tools ∩ права родителя (код, не
//!   модель); пустой список — наследует права родителя;
//! - бюджет ходов — жёстко (max_turns исполнителя);
//! - бюджет времени — [`DeadlineProvider`]/[`DeadlineDispatch`]: после
//!   дедлайна КАЖДЫЙ вызов модели и инструмента завершается ошибкой —
//!   цикл умирает штатным путём, без потоков и unsafe;
//! - вложенность: субагент порождает только с `allow_spawn: true` в
//!   своём agent.yaml (по умолчанию запрещено); абсолютный потолок
//!   глубины — 2 (родитель → ребёнок → внук), код, не модель;
//! - гейт и подтверждения — те же, что у родителя: мутирующие действия
//!   ребёнка спрашивают через тот же модал, deny-статика и jail общие;
//! - журнал: телеметрия ребёнка — вложенным инстансом
//!   `agent-<name>-<ts>` в тот же журнал запуска (аудит неразрывен).

use berimor_executors::agent_step::AgentStepExecutor;
use berimor_executors::tool_only::{DispatchError, ToolDispatch};
use berimor_model_pool::ModelPool;
use berimor_types::contract::Contract;
use berimor_types::executor::ModelProvider;
use berimor_types::model::{
    CompletionRequest, CompletionResponse, ModelError, ModelTierRequirement,
};
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Провайдер с дедлайном: после него каждый вызов — Unavailable, цикл
/// субагента завершается штатной ошибкой (бюджет времени, §20.17).
struct DeadlineProvider<'a> {
    inner: &'a dyn ModelProvider,
    deadline: Instant,
}

impl ModelProvider for DeadlineProvider<'_> {
    fn complete(&self, request: CompletionRequest) -> Result<CompletionResponse, ModelError> {
        if Instant::now() > self.deadline {
            return Err(ModelError::Unavailable(
                "бюджет времени субагента исчерпан".into(),
            ));
        }
        self.inner.complete(request)
    }
}

/// Диспетчер с дедлайном: после него вызовы инструментов — ошибка.
struct DeadlineDispatch<'a> {
    inner: &'a dyn ToolDispatch,
    allowed: Option<Vec<String>>,
    deadline: Instant,
}

impl ToolDispatch for DeadlineDispatch<'_> {
    fn call(&self, tool: &str, args: &Value) -> Result<Value, DispatchError> {
        if Instant::now() > self.deadline {
            return Err(DispatchError {
                tool: tool.into(),
                reason: "бюджет времени субагента исчерпан".into(),
            });
        }
        if let Some(allowed) = &self.allowed {
            if !allowed.iter().any(|t| t == tool) {
                return Err(DispatchError {
                    tool: tool.into(),
                    reason: "инструмент вне потолка субагента".into(),
                });
            }
        }
        self.inner.call(tool, args)
    }
}

/// Контекст исполнения поручений: всё, что нужно вложенному циклу.
pub(crate) struct AgentRunContext {
    pub pool: ModelPool,
    pub providers: Vec<(String, Arc<dyn ModelProvider + Send + Sync>)>,
    pub gate: Arc<berimor_capability::confirm::StandardCapability>,
    pub confirmer: Arc<dyn berimor_executors::tool_only::ConfirmationHandler + Send + Sync>,
    pub masker: Arc<berimor_secrets::Masker>,
    pub mode: berimor_types::capability::ConfirmationMode,
    pub storage_path: std::path::PathBuf,
    pub builtin_names: Vec<String>,
    /// Стек флагов allow_spawn исполняющихся агентов: вершина — права
    /// ТЕКУЩЕГО (порождающего) агента; длина стека = глубина вложенности.
    pub spawn_stack: Arc<std::sync::Mutex<Vec<bool>>>,
}

/// Абсолютный потолок глубины вложенности (0 = чат, 1 = ребёнок,
/// 2 = внук): выше — отказ независимо от флагов (код, не модель).
const MAX_SPAWN_DEPTH: usize = 2;

/// Диспетчер-обёртка: `agents.run` исполняет вложенным циклом, остальное
/// делегирует основной цепочке (встроенные → MCP → заглушки).
pub(crate) struct AgentRunDispatch {
    inner: Arc<dyn ToolDispatch + Send + Sync>,
    ctx: Arc<AgentRunContext>,
}

impl AgentRunDispatch {
    pub fn new(inner: Arc<dyn ToolDispatch + Send + Sync>, ctx: AgentRunContext) -> Self {
        Self {
            inner,
            ctx: Arc::new(ctx),
        }
    }

    fn run_agent(&self, args: &Value) -> Result<Value, DispatchError> {
        let name = args
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| DispatchError {
                tool: "agents.run".into(),
                reason: "аргумент name обязателен".into(),
            })?;
        let task = args
            .get("task")
            .and_then(Value::as_str)
            .ok_or_else(|| DispatchError {
                tool: "agents.run".into(),
                reason: "аргумент task обязателен".into(),
            })?;

        // Вложенность: вершина стека — права порождающего агента;
        // глубина ограничена абсолютным потолком.
        {
            let stack = self
                .ctx
                .spawn_stack
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if stack.len() >= MAX_SPAWN_DEPTH {
                return Err(DispatchError {
                    tool: "agents.run".into(),
                    reason: format!(
                        "потолок вложенности субагентов ({MAX_SPAWN_DEPTH}) — дальнейшее порождение запрещено"
                    ),
                });
            }
            if !stack.is_empty() && !stack.last().copied().unwrap_or(false) {
                return Err(DispatchError {
                    tool: "agents.run".into(),
                    reason: "порождающий субагент не имеет allow_spawn: true в своём agent.yaml"
                        .into(),
                });
            }
        }
        self.run_agent_inner(name, task)
    }

    fn run_agent_inner(&self, name: &str, task: &str) -> Result<Value, DispatchError> {
        let workspace = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        let defs = crate::agents::load_all(&workspace);
        let def = defs
            .iter()
            .find(|d| d.name == name)
            .ok_or_else(|| DispatchError {
                tool: "agents.run".into(),
                reason: format!(
                    "субагент '{name}' не найден. Установленные: {}",
                    defs.iter()
                        .map(|d| d.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            })?;

        // Права ребёнка — в стек на время исполнения (его собственные
        // порождения будут проверяться по ЕГО флагу).
        self.ctx
            .spawn_stack
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(def.allow_spawn);
        let outcome = self.run_agent_scoped(name, task, def);
        self.ctx
            .spawn_stack
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pop();
        outcome
    }

    fn run_agent_scoped(
        &self,
        name: &str,
        task: &str,
        def: &crate::agents::AgentDef,
    ) -> Result<Value, DispatchError> {
        // Потолок: agent.tools ∩ права родителя (встроенные имена).
        let ceiling = crate::agents::ceiling(def, &self.ctx.builtin_names);
        let deadline = Instant::now() + Duration::from_secs(def.max_wall_seconds);

        // Провайдеры с дедлайном + failover внутри класса.
        let requirement = match def.model_tier.as_deref() {
            Some("strong") => ModelTierRequirement::Strong,
            _ => ModelTierRequirement::Any,
        };
        let ranked = self.ctx.pool.select_ranked(requirement, None);
        if ranked.is_empty() {
            return Err(DispatchError {
                tool: "agents.run".into(),
                reason: format!("нет провайдера класса {requirement:?} для субагента"),
            });
        }
        // ВСЕ провайдеры с дедлайном (бюджет времени реально на вызовах
        // модели): исполнитель сам выбирает кандидата из пула — мапа
        // обязана содержать любого выбираемого, иначе «не подключён».
        let deadline_providers: Vec<(String, DeadlineProvider)> = self
            .ctx
            .providers
            .iter()
            .map(|(n, p)| {
                (
                    n.clone(),
                    DeadlineProvider {
                        inner: p.as_ref(),
                        deadline,
                    },
                )
            })
            .collect();

        // Журнал: вложенный инстанс в тот же журнал запуска.
        let instance_id = berimor_types::event::ProcessInstanceId(format!(
            "agent-{name}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0)
        ));
        let storage =
            berimor_storage::SqliteEventLog::open(&self.ctx.storage_path).map_err(|e| {
                DispatchError {
                    tool: "agents.run".into(),
                    reason: format!("журнал: {e}"),
                }
            })?;
        let telemetry_id = instance_id.clone();
        let on_attempt = move |kind: berimor_types::event::EventKind| {
            crate::run::audit_append(
                &storage,
                berimor_types::event::Event::new(telemetry_id.clone(), 1, kind, Value::Null),
            );
        };

        // Лёгкий контекст ребёнка: конституция/бюджет — те же, память
        // родителя не копируется (поручение автономно).
        let memory = berimor_context_engine::SimpleContextBuilder;
        // Внутренний диспетчер ребёнка — САМ AgentRunDispatch (не его
        // inner): agents.run ребёнка обязан пройти проверку вложенности
        // (allow_spawn/глубина), а не уйти в builtin как неизвестный
        // инструмент — найдено e2e вложенного порождения.
        let dispatch = DeadlineDispatch {
            inner: self,
            allowed: ceiling,
            deadline,
        };
        let mut providers_map: std::collections::HashMap<String, &dyn ModelProvider> =
            std::collections::HashMap::new();
        for (n, p) in &deadline_providers {
            providers_map.insert(n.clone(), p as &dyn ModelProvider);
        }
        let goal = if def.prompt.is_empty() {
            task.to_string()
        } else {
            format!(
                "[Инструкции субагента:\n{}]\n\nПоручение: {}",
                def.prompt, task
            )
        };
        let state = json!({
            "goal": goal,
            "history": [],
            "tools": self.ctx.builtin_names,
        });
        let agent = AgentStepExecutor {
            pool: &self.ctx.pool,
            providers: &providers_map,
            context: &memory,
            on_attempt: Some(&on_attempt),
            gate: self.ctx.gate.as_ref(),
            mode: self.ctx.mode,
            confirmer: self.ctx.confirmer.as_ref(),
            dispatch: &dispatch,
            secrets: self.ctx.masker.as_ref(),
            on_tool_turn: None,
            on_provider_switch: None,
            // BR-01: ребёнок получает перечень СВОИХ имён — потолок
            // agent.yaml; угадывание внутри потолка бессмысленно.
            tool_lines: def.tools.iter().map(|name| format!("- {name}")).collect(),
            observation_budget: berimor_executors::agent_step::DEFAULT_OBSERVATION_BUDGET,
        };
        let outcome = agent.execute(
            &instance_id.0,
            berimor_mediation::contracts::ChatReply::NAME,
            def.max_turns,
            false,
            false,
            &state,
            None,
        );
        match outcome {
            Ok(patch) => {
                let reply = patch
                    .changes
                    .get("reply")
                    .and_then(Value::as_str)
                    .unwrap_or("(пустой ответ субагента)");
                let reply = self.ctx.masker.mask_text(reply);
                Ok(json!({"content": reply}))
            }
            Err(err) => Err(DispatchError {
                tool: "agents.run".into(),
                reason: format!("субагент '{name}': {err}"),
            }),
        }
    }
}

impl ToolDispatch for AgentRunDispatch {
    fn call(&self, tool: &str, args: &Value) -> Result<Value, DispatchError> {
        if tool == "agents.run" {
            return self.run_agent(args);
        }
        self.inner.call(tool, args)
    }
}
