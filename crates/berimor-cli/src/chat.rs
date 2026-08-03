//! Интерактивный режим `berimor chat` (ROADMAP §20.11) — REPL поверх
//! свободного агентного цикла (AgentStep) со встроенными инструментами.
//!
//! Не новый исполнитель и не новая безопасность: каждый ход модели
//! проходит тот же путь, что в `berimor run` — Mediation хода
//! (AgentTurnDecision), capability-гейт вызова (deny-статика, jail,
//! режимы подтверждений с вопросом в терминал), маскировка наблюдений
//! и ответов, телеметрия попыток в тот же SQLite-журнал (инстанс
//! `chat-<ts>` — сессия читается `berimor trace` наравне с процессами).
//!
//! Slash-команды (§20.12) — служебный канал, НЕ уходящий модели:
//! /help, /config, /models, /models add (мастер пресетов + перезагрузка
//! рантайма с новым конфигом, история диалога сохраняется), /exit.
//!
//! Осознанные границы v1 (задокументированы, не скрыты):
//! - история диалога — в памяти процесса, между сессиями не
//!   персистируется (прошлые журналы читает Session-слой эпизодической
//!   памяти, но сама лента чата в state не восстанавливается);
//! - один вызов модели на ход агента, до 12 ходов на сообщение;
//! - self_critique/verify_actions выключены ради отзывчивости.

use crate::builtin_dispatch::builtin_policies;
use crate::config::{self, Config};
use crate::run::{build_executor_bundle, RunError};
use crate::setup;
use berimor_context_engine::memory_builder::MemoryContextBuilder;
use berimor_executors::agent_step::AgentStepExecutor;
use berimor_mediation::contracts::ChatReply;
use berimor_storage::{EventLog, SqliteEventLog};
use berimor_types::contract::Contract;
use berimor_types::event::{Event, EventKind, ProcessInstanceId};
use serde_json::{json, Value};
use std::io::Write;
use std::path::Path;

/// Ходов агента на одно сообщение пользователя — потолок, не цель;
/// каждый ход — до 4 вызовов модели (см. AgentStepExecutor::execute).
const MAX_TURNS_PER_MESSAGE: u32 = 12;

/// Исход сессии REPL: выход из чата или перезагрузка рантайма после
/// изменения конфигурации (`/models add`).
enum SessionOutcome {
    Exit,
    Reload,
}

/// Каталог инструментов для состояния агента: модель узнаёт имена и
/// формы аргументов из state (подсказка хода список не несёт —
/// см. agent_step.rs, «шагу доступен весь сконфигурированный набор»).
fn tools_catalog(config: &Config) -> Value {
    let mut tools = vec![
        json!({"name": "files.read", "args": {"path": "строка"}, "about": "прочитать файл (кап 1 МиБ, флаг truncated)"}),
        json!({"name": "files.write", "args": {"path": "строка", "content": "строка"}, "about": "записать файл (родитель обязан существовать)"}),
        json!({"name": "files.list", "args": {"path": "строка, по умолчанию \".\""}, "about": "листинг каталога (до 1000 записей)"}),
        json!({"name": "terminal.exec", "args": {"command": "строка"}, "about": "выполнить команду оболочки (30 сек, 64 КиБ вывода)"}),
        json!({"name": "http.fetch", "args": {"url": "строка"}, "about": "GET-запрос (приватные адреса запрещены, редиректы не следуются)"}),
    ];
    for stub in &config.tool_stubs {
        tools.push(json!({"name": stub.tool, "args": {}, "about": "объявленный оператором инструмент (заглушка)"}));
    }
    for server in &config.mcp_servers {
        tools.push(json!({"name": format!("{}.*", server.name), "args": {}, "about": "инструменты MCP-сервера"}));
    }
    Value::Array(tools)
}

fn print_help() {
    eprintln!("[berimor] команды чата:");
    eprintln!("  /help        — этот список");
    eprintln!("  /config      — эффективная конфигурация (слои: глобальный ← локальный)");
    eprintln!("  /models      — провайдеры моделей эффективного конфига");
    eprintln!("  /models add  — мастер пресетов (kimi, deepseek, openai, claude,");
    eprintln!("                 ollama, llamacpp, lmstudio) → глобальный конфиг,");
    eprintln!("                 рантайм перезагружается, история сохраняется");
    eprintln!("  /exit, /quit — завершить (Ctrl+D тоже)");
    eprintln!("[berimor] всё остальное — сообщение агенту.");
}

fn print_config(config: &Config) {
    eprintln!("[berimor] конфигурация (глобальный слой ← локальный):");
    eprintln!("  журнал: {}", config.storage_path.display());
    eprintln!("  режим подтверждений: {:?}", config.confirmation_mode);
    eprintln!("  провайдеров: {}", config.providers.len());
    eprintln!("  заглушек инструментов: {}", config.tool_stubs.len());
    eprintln!("  MCP-серверов: {}", config.mcp_servers.len());
}

fn print_models(config: &Config) {
    if config.providers.is_empty() {
        eprintln!("[berimor] провайдеры не настроены — /models add");
        return;
    }
    eprintln!("[berimor] провайдеры моделей:");
    for provider in &config.providers {
        let endpoint = if provider.model_path.is_some() {
            "локальный инференс (GGUF)".to_string()
        } else {
            provider.base_url.clone()
        };
        eprintln!(
            "  {} — {} ({:?}), {}",
            provider.name, provider.model_id, provider.tier, endpoint
        );
    }
}

pub(crate) fn cmd_chat(explicit_config: Option<&Path>) -> Result<(), RunError> {
    // История переживает перезагрузку рантайма (`/models add`): лента
    // диалога — не часть бандла, терять её из-за смены конфига нельзя.
    let mut history: Vec<Value> = Vec::new();
    loop {
        let config =
            config::load(explicit_config).map_err(|err| RunError::BadInput(err.to_string()))?;
        match run_repl(&config, &mut history)? {
            SessionOutcome::Exit => return Ok(()),
            SessionOutcome::Reload => {
                eprintln!("[berimor] конфигурация перечитана, рантайм пересобран");
            }
        }
    }
}

fn run_repl(config: &Config, history: &mut Vec<Value>) -> Result<SessionOutcome, RunError> {
    let bundle = build_executor_bundle(config)?;
    let storage =
        SqliteEventLog::open(&config.storage_path).map_err(|err| RunError::OpenStorage {
            path: config.storage_path.clone(),
            reason: err.to_string(),
        })?;
    let providers = bundle.providers();

    // Телеметрия Mediation — в тот же журнал, инстанс сессии чата
    // (security-model.md §5: аудит-след виден через berimor trace).
    let instance_id = ProcessInstanceId(format!(
        "chat-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0)
    ));
    let telemetry_id = instance_id.clone();
    let on_attempt = |kind: EventKind| {
        let _ = storage.append(Event::new(telemetry_id.clone(), 1, kind, Value::Null));
    };

    let memory_context = MemoryContextBuilder {
        episodic: &storage,
        skills: &bundle.skills,
        session_search_limit: config.memory.session_search_limit,
        entity_graph: config
            .memory
            .entity_graph
            .then_some(&storage as &dyn berimor_storage::EntityGraphStore),
        masker: Some(bundle.masker.as_ref()),
    };

    let agent = AgentStepExecutor {
        pool: &bundle.pool,
        providers: &providers,
        context: &memory_context,
        on_attempt: Some(&on_attempt),
        gate: bundle.gate.as_ref(),
        mode: config.confirmation_mode,
        confirmer: bundle.confirmer.as_ref(),
        dispatch: bundle.dispatch.as_ref(),
        secrets: bundle.masker.as_ref(),
    };

    let catalog = tools_catalog(config);
    let builtin_names: Vec<String> = builtin_policies().iter().map(|(n, _)| n.clone()).collect();
    eprintln!(
        "[berimor] chat: рабочая область — текущая директория (jail), режим подтверждений: {:?}",
        config.confirmation_mode
    );
    eprintln!(
        "[berimor] инструменты: {}{}; сессия в журнале: {}",
        builtin_names.join(", "),
        if config.tool_stubs.is_empty() && config.mcp_servers.is_empty() {
            String::new()
        } else {
            " + конфигурация оператора".to_string()
        },
        instance_id.0
    );
    eprintln!("[berimor] /help — команды; завершение: /exit или Ctrl+D");

    let stdin = std::io::stdin();
    loop {
        eprint!("вы> ");
        let _ = std::io::stderr().flush();
        let mut line = String::new();
        let read = stdin
            .read_line(&mut line)
            .map_err(|err| RunError::InstanceLease(format!("чтение stdin: {err}")))?;
        if read == 0 {
            eprintln!("[berimor] EOF — сессия завершена");
            return Ok(SessionOutcome::Exit);
        }
        let message = line.trim();
        if message.is_empty() {
            continue;
        }

        // Slash-команды — служебный канал, модели не уходят.
        if let Some(command) = message.strip_prefix('/') {
            match command {
                "exit" | "quit" => {
                    eprintln!("[berimor] сессия завершена");
                    return Ok(SessionOutcome::Exit);
                }
                "help" => print_help(),
                "config" => print_config(config),
                "models" => print_models(config),
                "models add" => {
                    if let Err(err) = setup::run_wizard() {
                        eprintln!("[berimor] мастер настройки: {err}");
                        continue;
                    }
                    return Ok(SessionOutcome::Reload);
                }
                _ => eprintln!("[berimor] неизвестная команда /{command} — /help"),
            }
            continue;
        }

        let state = json!({
            "goal": message,
            "history": *history,
            "tools": catalog,
        });
        match agent.execute(
            "chat",
            ChatReply::NAME,
            MAX_TURNS_PER_MESSAGE,
            false,
            false,
            &state,
            None,
        ) {
            Ok(patch) => {
                let reply = patch
                    .changes
                    .get("reply")
                    .and_then(Value::as_str)
                    .unwrap_or("(пустой ответ)")
                    .to_string();
                // Ответ модели — пользователю через маскировщик (та же
                // граница, что tool-вывод и human_gate в run).
                let reply = bundle.masker.mask_text(&reply);
                println!("berimor> {reply}");
                history.push(json!({"role": "user", "content": message}));
                history.push(json!({"role": "assistant", "content": reply}));
            }
            Err(err) => {
                // Ошибка хода — не смерть сессии: пользователь видит
                // причину (маскированную) и продолжает диалог.
                eprintln!(
                    "[berimor] ход завершился ошибкой: {}",
                    bundle.masker.mask_text(&err.to_string())
                );
            }
        }
    }
}
