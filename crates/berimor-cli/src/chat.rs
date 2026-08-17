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
use crate::chat_ui::{self, Theme};
use crate::config::{self, Config};
use crate::i18n::{self, Locale};
use crate::run::{build_executor_bundle, RunError};
use crate::setup;
use berimor_context_engine::memory_builder::{FactsSource, MemoryContextBuilder};
use berimor_executors::agent_step::AgentStepExecutor;
use berimor_executors::structured_llm::StructuredLlm;
use berimor_mediation::contracts::{ChatReply, HistorySummary};
use berimor_storage::{EventLog, SqliteEventLog};
use berimor_types::contract::Contract;
use berimor_types::event::{Event, EventKind, ProcessInstanceId};
use berimor_types::model::ModelTierRequirement;
use serde_json::{json, Value};
use std::io::IsTerminal;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Ходов агента на одно сообщение пользователя — потолок, не цель;
/// каждый ход — до 4 вызовов модели (см. AgentStepExecutor::execute).
/// Потолок ходов на сообщение чата — из `[agent] max_turns` (0.34.0;
/// прежняя константа 12 убивала легитимную многократную работу —
/// анализ проекта. Страж зацикливания — отдельно, в исполнителе).
fn max_turns_per_message(config: &Config) -> u32 {
    config.agent.max_turns
}

/// Исход сессии REPL: выход из чата или перезагрузка рантайма после
/// изменения конфигурации (`/models add`).
enum SessionOutcome {
    Exit,
    Reload,
}

/// Каталог инструментов для состояния агента: модель узнаёт имена и
/// формы аргументов из state (подсказка хода список не несёт —
/// см. agent_step.rs, «шагу доступен весь сконфигурированный набор»).
pub(crate) fn tools_catalog(config: &Config) -> Value {
    let mut tools = vec![
        json!({"name": "files.read", "args": {"path": "строка"}, "about": "прочитать файл (кап 1 МиБ, флаг truncated)"}),
        json!({"name": "files.write", "args": {"path": "строка", "content": "строка"}, "about": "записать файл целиком (родитель обязан существовать; перед записью — авто-снапшот)"}),
        json!({"name": "files.edit", "args": {"path": "строка", "old_string": "строка", "new_string": "строка", "replace_all": "bool?"}, "about": "точечная правка по строковому якорю (контроль уникальности)"}),
        json!({"name": "files.list", "args": {"path": "строка, по умолчанию \".\""}, "about": "листинг каталога (до 1000 записей)"}),
        json!({"name": "files.search", "args": {"pattern": "regex или glob", "mode": "content|files", "path": "строка?", "glob": "строка?", "limit": "число?"}, "about": "поиск по файлам: regex по содержимому (строки+контекст) или glob по именам; .git/target/node_modules пропускаются"}),
        json!({"name": "terminal.exec", "args": {"command": "строка"}, "about": "выполнить команду оболочки (30 сек, 64 КиБ вывода)"}),
        json!({"name": "terminal.start", "args": {"command": "строка"}, "about": "фоновый процесс (до 32) — для серверов и долгих задач; id в ответе"}),
        json!({"name": "terminal.output", "args": {"id": "число", "offset": "число?"}, "about": "вывод фонового процесса (stdout/stderr/running)"}),
        json!({"name": "terminal.kill", "args": {"id": "число"}, "about": "остановить фоновый процесс"}),
        json!({"name": "http.fetch", "args": {"url": "строка"}, "about": "GET-запрос (приватные адреса запрещены, редиректы не следуются)"}),
        json!({"name": "web.search", "args": {"query": "строка", "limit": "число?"}, "about": "поисковая выдача DuckDuckGo (заголовок/ссылка/сниппет)"}),
        json!({"name": "vcs.git", "args": {"op": "status|diff|log|show", "path": "строка?", "limit": "число?"}, "about": "git только на чтение (хелперы репозитория отключены)"}),
        json!({"name": "todo.read", "args": {}, "about": "список задач сессии (.berimor/todo.json)"}),
        json!({"name": "todo.write", "args": {"items": "[{id, content, status}]"}, "about": "заменить список задач (status: pending|in_progress|completed|cancelled)"}),
        json!({"name": "human.ask", "args": {"question": "строка", "options": "[строка]?"}, "about": "вопрос пользователю из цикла (свободный ответ)"}),
        json!({"name": "memory.search", "args": {"query": "строка", "limit": "число?"}, "about": "поиск фактов семантической памяти"}),
        json!({"name": "memory.save", "args": {"content": "строка", "topic": "строка?"}, "about": "записать факт (выключено по умолчанию: [memory] tool_writes)"}),
        json!({"name": "session.search", "args": {"query": "строка", "limit": "число?", "role": "user|assistant?"}, "about": "поиск по лентам прошлых сессий (excerpt с контекстом)"}),
        json!({"name": "snapshot.list", "args": {"limit": "число?"}, "about": "метки авто-снапшотов файлов (перед перезаписями)"}),
        json!({"name": "snapshot.restore", "args": {"id": "строка", "path": "строка?"}, "about": "откат файла(ов) из снапшота (сам со снапшотом)"}),
        json!({"name": "agents.run", "args": {"name": "имя субагента", "task": "поручение"}, "about": "поручить задачу субагенту (свои потолок и бюджет; berimor agent list)"}),
    ];
    for stub in &config.tool_stubs {
        tools.push(json!({"name": stub.tool, "args": {}, "about": "объявленный оператором инструмент (заглушка)"}));
    }
    // Инструменты установленных плагинов (§20.18) — из ACL-манифестов.
    let plugin_runtime = crate::plugin_runtime::PluginRuntimeDispatch::scan(
        &crate::plugin_install::plugins_root_dir(),
    );
    for decl in plugin_runtime.tool_decls() {
        tools.push(
            json!({"name": decl.name, "args": {}, "about": format!("плагин: {}", decl.description)}),
        );
    }
    for server in &config.mcp_servers {
        tools.push(json!({"name": format!("{}.*", server.name), "args": {}, "about": "инструменты MCP-сервера"}));
    }
    Value::Array(tools)
}

/// Однострочные описания инструментов для промпта свободного цикла
/// (BR-01, полевой тест 2026-08-14): промпт agent_step перечисляет
/// доступные имена — модель не угадывает (list_files вместо files.list).
pub(crate) fn tool_prompt_lines(config: &Config) -> Vec<String> {
    tools_catalog(config)
        .as_array()
        .map(|tools| {
            tools
                .iter()
                .map(|t| {
                    let name = t["name"].as_str().unwrap_or_default();
                    let about = t["about"].as_str().unwrap_or_default();
                    let args = t["args"]
                        .as_object()
                        .map(|o| o.keys().map(|k| k.as_str()).collect::<Vec<_>>().join(", "))
                        .unwrap_or_default();
                    format!("- {name} {{{args}}} — {about}")
                })
                .collect()
        })
        .unwrap_or_default()
}

fn print_help() {
    eprintln!("[berimor] команды чата:");
    eprintln!("  /help        — этот список");
    eprintln!("  /config      — эффективная конфигурация (слои: глобальный ← локальный)");
    eprintln!("  /models      — провайдеры моделей эффективного конфига");
    eprintln!("  /models add  — мастер пресетов (kimi, moonshot, deepseek, openai,");
    eprintln!("                 claude, ollama, llamacpp, lmstudio, vllm, textgenwebui,");
    eprintln!("                 koboldcpp) → глобальный конфиг, рантайм перезагружается,");
    eprintln!("                 история сохраняется");
    eprintln!("  /sessions    — живые сессии хоста (реестр журнала)");
    eprintln!("  /tell <id> <текст> — сообщение сессии (персистентная почта)");
    eprintln!("  /broadcast <текст> — сообщение всем живым сессиям");
    eprintln!("  /exit, /quit — завершить (Ctrl+D тоже)");
    eprintln!("  /config locale <код> — локаль интерфейса TUI (ru, en, de, fr, es, zh-CN, ja, ko)");
    eprintln!("  /mouse, /copy — только в полноэкранном TUI (мышь/буфер обмена);");
    eprintln!("                  в построчном режиме мышь не захватывается");
    eprintln!("[berimor] всё остальное — сообщение агенту.");
}

fn print_config(config: &Config) {
    eprintln!("[berimor] конфигурация (глобальный слой ← локальный):");
    eprintln!("  журнал: {}", config.storage_path.display());
    eprintln!("  режим подтверждений: {:?}", config.confirmation_mode);
    eprintln!("  провайдеров: {}", config.providers.len());
    eprintln!("  заглушек инструментов: {}", config.tool_stubs.len());
    eprintln!("  MCP-серверов: {}", config.mcp_servers.len());
    let locale = Locale::resolve(config.ui.locale.as_deref());
    eprintln!(
        "  локаль интерфейса: {} ({})",
        locale.native_name(),
        locale.code()
    );
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

/// Один ход агента с собственным рантаймом (TUI-воркер, §20.14):
/// bundle/журнал/исполнитель строятся в потоке вызова из принадлежащего
/// потоку конфига — никаких заимствований между потоками, «перезагрузка»
/// после смены конфига бесплатна. События инструментов — в канал UI.
/// Ошибка — маскированная строка (безопасно показывать и логировать).
/// Подтверждения в TUI (§20.14): TerminalConfirmer писал бы в stdout
/// поверх alternate screen — каша в поле ввода (репорт 2026-08-03).
/// Вместо этого — модал в TUI: запрос по каналу, ответ ждём блокируясь
/// в воркере. Канал умер — «нет» (подтверждение opt-in по определению).
struct TuiConfirmer<'a> {
    masker: &'a berimor_secrets::Masker,
    tx: std::sync::mpsc::Sender<crate::chat_tui::WorkerMsg>,
    answer_rx: std::sync::mpsc::Receiver<crate::chat_tui::ConfirmAnswer>,
    /// Разрешения «для сессии» — общие с UI набор (0.14.0).
    session_grants: std::sync::Arc<std::sync::Mutex<std::collections::HashSet<String>>>,
}

impl berimor_executors::tool_only::ConfirmationHandler for TuiConfirmer<'_> {
    fn confirm(&self, action: &berimor_types::capability::ProposedAction, reason: &str) -> bool {
        // Разрешение «для сессии» уже дано — работаем молча (директива:
        // «если есть разрешение — не ебать пользователю мозги»).
        if let Ok(grants) = self.session_grants.lock() {
            if grants.contains(&action.tool) || grants.contains("*") {
                return true;
            }
        }
        let text = self
            .masker
            .mask_text(&format!("{reason}\n{} {}", action.tool, action.args));
        let _ = self
            .tx
            .send(crate::chat_tui::WorkerMsg::ConfirmRequest(text));
        match self
            .answer_rx
            .recv()
            .unwrap_or(crate::chat_tui::ConfirmAnswer::Deny)
        {
            crate::chat_tui::ConfirmAnswer::Once => true,
            crate::chat_tui::ConfirmAnswer::Session => {
                if let Ok(mut grants) = self.session_grants.lock() {
                    grants.insert(action.tool.clone());
                }
                let _ = self.tx.send(crate::chat_tui::WorkerMsg::Sys(format!(
                    "разрешение на сессию: {}",
                    action.tool
                )));
                true
            }
            crate::chat_tui::ConfirmAnswer::Project => {
                let workspace =
                    std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
                let persisted = crate::config::append_project_allow(&workspace, &action.tool);
                if let Ok(mut grants) = self.session_grants.lock() {
                    grants.insert(action.tool.clone());
                }
                let note = match persisted {
                    Ok(()) => format!(
                        "разрешение для проекта: {} (записано в .berimor/allow)",
                        action.tool
                    ),
                    Err(err) => format!(
                        "разрешение для проекта: {} — НЕ удалось записать .berimor/allow ({err}); действует до конца сессии",
                        action.tool
                    ),
                };
                let _ = self.tx.send(crate::chat_tui::WorkerMsg::Sys(note));
                true
            }
            crate::chat_tui::ConfirmAnswer::ProjectAll => {
                let workspace =
                    std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
                let persisted = crate::config::append_project_allow(&workspace, "*");
                if let Ok(mut grants) = self.session_grants.lock() {
                    grants.insert("*".to_string());
                }
                let note = match persisted {
                    Ok(()) => "широкое разрешение для проекта: все инструменты (записано в .berimor/allow)".to_string(),
                    Err(err) => format!(
                        "широкое разрешение НЕ записано ({err}); действует до конца сессии"
                    ),
                };
                let _ = self.tx.send(crate::chat_tui::WorkerMsg::Sys(note));
                true
            }
            crate::chat_tui::ConfirmAnswer::Deny => false,
        }
    }
}

/// human.ask в TUI (B7, spec builtin-tools-waves): вопрос — модал с
/// полем ввода (WorkerMsg::AskRequest), ответ ждём блокируясь в воркере
/// — тот же канальный паттерн, что TuiConfirmer. Смерть канала —
/// ошибка asker'а (инструмент получает DispatchError, цикл не висит).
struct TuiAsker {
    tx: std::sync::mpsc::Sender<crate::chat_tui::WorkerMsg>,
    // Mutex: mpsc::Receiver не Sync, а HumanAsker требует Send+Sync.
    answer_rx: std::sync::Mutex<std::sync::mpsc::Receiver<Result<String, String>>>,
}

impl crate::builtin_human::HumanAsker for TuiAsker {
    fn ask(&self, question: &str) -> Result<String, String> {
        self.tx
            .send(crate::chat_tui::WorkerMsg::AskRequest(question.to_string()))
            .map_err(|_| "канал UI закрыт".to_string())?;
        let rx = self
            .answer_rx
            .lock()
            .map_err(|_| "блокировка канала ответа".to_string())?;
        rx.recv()
            .map_err(|_| "канал ответа UI закрыт".to_string())?
    }
}

/// Триггер скилла → расширенное сообщение + потолок + имя (общий для
/// TUI и REPL — §20.16; триггер вычисляет код, не модель).
pub(crate) fn resolve_skill_trigger(
    skills: &[crate::skills::Skill],
    message: &str,
) -> Option<(String, Option<Vec<String>>, String)> {
    let skill = crate::skills::match_trigger(skills, message)?;
    let ceiling = if skill.tools.is_empty() {
        None
    } else {
        Some(skill.tools.clone())
    };
    let augmented = format!(
        "[Активен скилл «{}» v{}. Следуй его инструкциям:\n{}]\n\nЗапрос пользователя: {}",
        skill.name, skill.version, skill.body, message
    );
    Some((augmented, ceiling, skill.name.clone()))
}

/// Потолок инструментов активного скилла (§20.16): вызов вне списка —
/// ошибка диспетча, модель видит причину. Пересечение с правами
/// сессии вычислено снаружи кодом (триггер — не модель).
struct CeilingDispatch<'a> {
    inner: &'a dyn berimor_executors::tool_only::ToolDispatch,
    allowed: &'a [String],
}

impl berimor_executors::tool_only::ToolDispatch for CeilingDispatch<'_> {
    fn call(
        &self,
        tool: &str,
        args: &serde_json::Value,
    ) -> Result<serde_json::Value, berimor_executors::tool_only::DispatchError> {
        if !self.allowed.iter().any(|t| t == tool) {
            return Err(berimor_executors::tool_only::DispatchError {
                tool: tool.into(),
                reason: "инструмент вне потолка активного скилла".into(),
            });
        }
        self.inner.call(tool, args)
    }
}

/// Каналы ответов UI на ход (TUI): подтверждения гейта и ответы
/// human.ask. None-поля — REPL/пайпы (TerminalConfirmer/StdinAsker).
pub(crate) struct TurnChannels {
    pub answer_rx: Option<std::sync::mpsc::Receiver<crate::chat_tui::ConfirmAnswer>>,
    pub ask_rx: Option<std::sync::mpsc::Receiver<Result<String, String>>>,
}

pub(crate) fn execute_turn(
    config: &Config,
    conversation: Vec<Value>,
    message: String,
    tx: std::sync::mpsc::Sender<crate::chat_tui::WorkerMsg>,
    channels: TurnChannels,
    session_grants: std::sync::Arc<std::sync::Mutex<std::collections::HashSet<String>>>,
    tool_ceiling: Option<Vec<String>>,
) -> Result<String, String> {
    let TurnChannels { answer_rx, ask_rx } = channels;
    let bundle = build_executor_bundle(config).map_err(|err| err.to_string())?;
    let storage = SqliteEventLog::open(&config.storage_path).map_err(|err| err.to_string())?;
    let providers = bundle.providers();
    let instance_id = ProcessInstanceId(format!(
        "chat-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0)
    ));
    let telemetry_id = instance_id.clone();
    let on_attempt = |kind: EventKind| {
        crate::run::audit_append(
            &storage,
            Event::new(telemetry_id.clone(), 1, kind, Value::Null),
        );
    };
    let facts_embed = crate::run::facts_embed_fn(config.memory.embeddings);
    let memory_context = MemoryContextBuilder {
        episodic: &storage,
        skills: &bundle.skills,
        session_search_limit: if config.memory.session_context {
            config.memory.session_search_limit
        } else {
            0
        },
        entity_graph: config
            .memory
            .entity_graph
            .then_some(&storage as &dyn berimor_storage::EntityGraphStore),
        // prompt-next-wave.md задача 1: слой Facts ищет по `state.goal` —
        // тому самому сообщению пользователя, ради которого писалась
        // задача (`execute_turn` собирает state с "goal": message ниже).
        facts: facts_embed.as_deref().map(|embed| FactsSource {
            store: &storage,
            embed,
            limit: config.memory.facts_search_limit,
        }),
        masker: Some(bundle.masker.as_ref()),
    };
    let on_tool_turn = |tool: &str, args: &Value, _observation: &Value, ok: bool| {
        let _ = tx.send(crate::chat_tui::WorkerMsg::ToolTurn(format!(
            "{} {}({})",
            if ok { "✓" } else { "✗" },
            tool,
            crate::chat_ui::summarize_args(args)
        )));
    };
    let tui_confirmer = answer_rx.map(|rx| TuiConfirmer {
        masker: bundle.masker.as_ref(),
        tx: tx.clone(),
        answer_rx: rx,
        session_grants,
    });
    let confirmer: &dyn berimor_executors::tool_only::ConfirmationHandler = match &tui_confirmer {
        Some(tui) => tui,
        None => bundle.confirmer.as_ref(),
    };
    // human.ask (B7): обёртка диспетчера — TUI-asker при живом канале,
    // иначе stdin (REPL/пайпы, прецедент TerminalConfirmer). memory.*
    // (C8) — обёртка с путём хранилища и флагом `[memory] tool_writes`.
    let stdin_asker = crate::builtin_human::StdinAsker;
    let memory_dispatch = crate::builtin_memory::MemoryToolDispatch {
        storage_path: config.storage_path.clone(),
        allow_writes: config.memory.tool_writes,
        inner: bundle.dispatch.as_ref(),
        masker: Some(bundle.masker.as_ref()),
    };
    let tui_asker = ask_rx.map(|rx| TuiAsker {
        tx: tx.clone(),
        answer_rx: std::sync::Mutex::new(rx),
    });
    let asker_ref: &dyn crate::builtin_human::HumanAsker = match &tui_asker {
        Some(a) => a,
        None => &stdin_asker,
    };
    let ask_dispatch = crate::builtin_human::HumanAskDispatch {
        asker: asker_ref,
        inner: &memory_dispatch,
    };
    let ceiling_dispatch = tool_ceiling.as_deref().map(|allowed| CeilingDispatch {
        inner: &ask_dispatch,
        allowed,
    });
    let agent = AgentStepExecutor {
        pool: &bundle.pool,
        providers: &providers,
        context: &memory_context,
        on_attempt: Some(&on_attempt),
        gate: bundle.gate.as_ref(),
        mode: config.confirmation_mode,
        confirmer,
        dispatch: ceiling_dispatch
            .as_ref()
            .map(|d| d as &dyn berimor_executors::tool_only::ToolDispatch)
            .unwrap_or(&ask_dispatch),
        secrets: bundle.masker.as_ref(),
        on_tool_turn: Some(&on_tool_turn),
        on_provider_switch: None,
        tool_lines: crate::chat::tool_prompt_lines(config),
    };
    let state = json!({
        "goal": message,
        "history": conversation,
        "tools": tools_catalog(config),
    });
    agent
        .execute(
            "chat",
            ChatReply::NAME,
            max_turns_per_message(config),
            false,
            false,
            &state,
            None,
        )
        .map(|patch| {
            let reply = patch
                .changes
                .get("reply")
                .and_then(Value::as_str)
                .unwrap_or("(пустой ответ)")
                .to_string();
            bundle.masker.mask_text(&reply)
        })
        .map_err(|err| bundle.masker.mask_text(&err.to_string()))
}

/// §20.22 v3: system-заметка модели об изменении наблюдаемого файла.
fn file_changed_note(envelope: &berimor_storage::Envelope) -> Value {
    json!({
        "role": "system",
        "content": format!(
            "[уведомление] Файл {}, который вы читали, изменён другой сессией ({} через {}). Если он важен для текущей задачи — перечитайте через files.read.",
            envelope.payload["path"].as_str().unwrap_or("?"),
            envelope.payload["by_session"].as_str().unwrap_or("?"),
            envelope.payload["op"].as_str().unwrap_or("?"),
        )
    })
}

pub(crate) fn cmd_chat(explicit_config: Option<&Path>) -> Result<(), RunError> {
    // Полноэкранный TUI — только на настоящем терминале (§20.14);
    // пайпы/скрипты/e2e — построчный REPL ниже (его поведение не менялось).
    if std::io::stdin().is_terminal() && std::io::stderr().is_terminal() {
        return crate::chat_tui::run_tui(explicit_config);
    }
    // История переживает перезагрузку рантайма (`/models add`): лента
    // диалога — не часть бандла, терять её из-за смены конфига нельзя.
    // Плюс подхват ленты прошлых сессий области (§20.15).
    let workspace = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    // Реестр сессий (§20.22 v2): open при старте, close при выходе —
    // best-effort (журнал недоступен — чат не падает из-за реестра).
    let session_id = crate::sessions::new_session_id();
    let session_journal: Option<std::sync::Arc<berimor_storage::SqliteEventLog>> =
        config::load(explicit_config)
            .ok()
            .and_then(|c| berimor_storage::SqliteEventLog::open(&c.storage_path).ok())
            .map(std::sync::Arc::new);
    if let Some(journal) = &session_journal {
        let _ = crate::sessions::record_open(journal, &session_id, "chat");
    }
    let mut history: Vec<Value> = crate::chat_history::load(&workspace);
    if !history.is_empty() {
        eprintln!(
            "[berimor] подхвачено сообщений прошлых сессий: {}",
            history.len()
        );
    }
    loop {
        let config =
            config::load(explicit_config).map_err(|err| RunError::BadInput(err.to_string()))?;
        match run_repl(&config, &mut history, session_journal.as_ref(), &session_id)? {
            SessionOutcome::Exit => {
                if let Some(journal) = &session_journal {
                    let _ = crate::sessions::record_closed(journal, &session_id);
                }
                return Ok(());
            }
            SessionOutcome::Reload => {
                eprintln!("[berimor] конфигурация перечитана, рантайм пересобран");
            }
        }
    }
}

fn run_repl(
    config: &Config,
    history: &mut Vec<Value>,
    session_journal: Option<&std::sync::Arc<SqliteEventLog>>,
    session_id: &str,
) -> Result<SessionOutcome, RunError> {
    let bundle = crate::run::build_executor_bundle_with_session(
        config,
        session_journal.map(|j| (j.clone(), session_id.to_string())),
        false,
    )?;
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
        crate::run::audit_append(
            &storage,
            Event::new(telemetry_id.clone(), 1, kind, Value::Null),
        );
    };

    let facts_embed = crate::run::facts_embed_fn(config.memory.embeddings);
    let memory_context = MemoryContextBuilder {
        episodic: &storage,
        skills: &bundle.skills,
        session_search_limit: if config.memory.session_context {
            config.memory.session_search_limit
        } else {
            0
        },
        entity_graph: config
            .memory
            .entity_graph
            .then_some(&storage as &dyn berimor_storage::EntityGraphStore),
        facts: facts_embed.as_deref().map(|embed| FactsSource {
            store: &storage,
            embed,
            limit: config.memory.facts_search_limit,
        }),
        masker: Some(bundle.masker.as_ref()),
    };

    // Живой вывод вызовов инструментов (§20.13): презентационный канал
    // исполнителя — аргументы и наблюдения приходят замаскированными.
    let theme = Theme::detect();
    let on_tool_turn = |tool: &str, args: &Value, _observation: &Value, ok: bool| {
        chat_ui::print_tool_turn(&theme, tool, &chat_ui::summarize_args(args), ok);
    };

    // REPL-ветка: те же обёртки, что у TUI (B7 human.ask — StdinAsker,
    // C8 memory.* — флаг `[memory] tool_writes`).
    let repl_memory_dispatch = crate::builtin_memory::MemoryToolDispatch {
        storage_path: config.storage_path.clone(),
        allow_writes: config.memory.tool_writes,
        inner: bundle.dispatch.as_ref(),
        masker: Some(bundle.masker.as_ref()),
    };
    let repl_stdin_asker = crate::builtin_human::StdinAsker;
    let ask_dispatch_repl = crate::builtin_human::HumanAskDispatch {
        asker: &repl_stdin_asker,
        inner: &repl_memory_dispatch,
    };

    let agent = AgentStepExecutor {
        pool: &bundle.pool,
        providers: &providers,
        context: &memory_context,
        on_attempt: Some(&on_attempt),
        gate: bundle.gate.as_ref(),
        mode: config.confirmation_mode,
        confirmer: bundle.confirmer.as_ref(),
        dispatch: &ask_dispatch_repl,
        secrets: bundle.masker.as_ref(),
        on_tool_turn: Some(&on_tool_turn),
        on_provider_switch: None,
        tool_lines: tool_prompt_lines(config),
    };

    let catalog = tools_catalog(config);
    let builtin_names: Vec<String> = builtin_policies().iter().map(|(n, _)| n.clone()).collect();
    let tools_summary = format!(
        "{}{}",
        builtin_names.join(", "),
        if config.tool_stubs.is_empty() && config.mcp_servers.is_empty() {
            String::new()
        } else {
            " + конфигурация оператора".to_string()
        }
    );
    let workspace = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| ".".to_string());
    chat_ui::print_banner(&theme, &workspace, &tools_summary, &instance_id.0);

    // Скилы сессии (§20.16): триггер — кодом, потолок — фильтром хода.
    let chat_skills = crate::skills::load_all(&std::env::current_dir().unwrap_or_default());
    let stdin = std::io::stdin();
    loop {
        eprint!("{} ", theme.green("›"));
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
        if let Some(journal) = session_journal {
            let _ = crate::sessions::record_heartbeat(journal, session_id);
            // §20.22 v2 шаг 2: входящие уведомления (file.changed и др.) —
            // показ на границе хода, без фоновых потоков (дизайн swarm).
            for envelope in crate::sessions::drain_envelopes(journal, session_id) {
                if envelope.topic == crate::sessions::TOPIC_FILE_CHANGED {
                    eprintln!(
                        "[berimor] ⚠ {} изменён сессией {} ({})",
                        envelope.payload["path"].as_str().unwrap_or("?"),
                        envelope.payload["by_session"].as_str().unwrap_or("?"),
                        envelope.payload["op"].as_str().unwrap_or("?"),
                    );
                    // §20.22 v3: модель узнаёт об изменении, а не только
                    // человек (jcode swarm: «агент B, файл переписан»).
                    // Перечитать — решает модель через обычный gated
                    // files.read; сырой diff не инъектируется (граница
                    // доверия контента).
                    history.push(file_changed_note(&envelope));
                } else if envelope.topic == crate::sessions::TOPIC_SESSION_MESSAGE {
                    eprintln!(
                        "[berimor] ✉ {}: {}",
                        envelope.from,
                        envelope.payload["text"].as_str().unwrap_or("?")
                    );
                } else {
                    eprintln!(
                        "[berimor] сообщение от {} ({})",
                        envelope.from, envelope.topic
                    );
                }
            }
        }
        if message.is_empty() {
            continue;
        }

        // Ход после разбора команд/триггеров: (сообщение, потолок).
        // Slash-команды — служебный канал, модели не уходят.
        let turn: (String, Option<Vec<String>>) = if let Some(command) = message.strip_prefix('/') {
            // §20.22 v2 шаг 3: /tell и /broadcast — команды с аргументами,
            // разбираются до точного match по имени.
            if let Some(rest) = command.strip_prefix("tell ") {
                let Some((target, text)) = rest.split_once(' ') else {
                    eprintln!("[berimor] /tell <сессия> <текст> — /sessions для списка");
                    continue;
                };
                match session_journal {
                    Some(journal) => {
                        match crate::sessions::send_message(journal, session_id, target, text) {
                            Ok(()) => eprintln!("[berimor] ✉ → {target}"),
                            Err(err) => eprintln!("[berimor] {err}"),
                        }
                    }
                    None => eprintln!("[berimor] почта недоступна: журнал не открыт"),
                }
                continue;
            }
            if let Some(text) = command.strip_prefix("broadcast ") {
                match session_journal {
                    Some(journal) => {
                        match crate::sessions::broadcast_message(journal, session_id, text) {
                            Ok(0) => eprintln!("[berimor] живых сессий-получателей нет"),
                            Ok(n) => eprintln!("[berimor] ✉ → {n} сессиям"),
                            Err(err) => eprintln!("[berimor] {err}"),
                        }
                    }
                    None => eprintln!("[berimor] почта недоступна: журнал не открыт"),
                }
                continue;
            }
            if command == "sessions" {
                if let Some(journal) = session_journal {
                    let events = journal
                        .replay(&berimor_types::event::ProcessInstanceId(
                            crate::sessions::SESSIONS_INSTANCE_ID.to_string(),
                        ))
                        .unwrap_or_default();
                    let sessions = crate::sessions::fold_sessions(&events);
                    let live: Vec<_> = sessions
                        .iter()
                        .filter(|s| !s.closed && s.pid_alive)
                        .collect();
                    if live.is_empty() {
                        eprintln!("[berimor] живых сессий нет");
                    }
                    for s in live {
                        let marker = if s.session_id == session_id {
                            " (вы)"
                        } else {
                            ""
                        };
                        eprintln!(
                            "  {} | {} | pid {} | {}{}",
                            s.session_id, s.command, s.pid, s.cwd, marker
                        );
                    }
                } else {
                    eprintln!("[berimor] реестр недоступен: журнал не открыт");
                }
                continue;
            }
            match command {
                "exit" | "quit" => {
                    eprintln!("[berimor] сессия завершена");
                    return Ok(SessionOutcome::Exit);
                }
                "help" => {
                    print_help();
                    continue;
                }
                "config" => {
                    print_config(config);
                    continue;
                }
                // /config locale [код] — в REPL без пикера: код
                // аргументом, без аргумента — перечень кодов (i18n,
                // 2026-08-12; меню TUI — палитра `/config ` + пикер).
                cmd if cmd == "config locale" || cmd.starts_with("config locale ") => {
                    let arg = cmd.strip_prefix("config locale").unwrap().trim();
                    let strings = i18n::strings(Locale::resolve(config.ui.locale.as_deref()));
                    if arg.is_empty() {
                        eprintln!(
                            "[berimor] {}ru, en, de, fr, es, zh-CN, ja, ko — /config locale <код>",
                            strings.sys_config_locale
                        );
                    } else {
                        match Locale::from_code(arg) {
                            Some(locale) => {
                                match setup::set_locale_in_local_config(locale.code()) {
                                    Ok(path) => eprintln!(
                                        "[berimor] {}{} ({}) {} — {path}",
                                        strings.sys_config_locale,
                                        locale.native_name(),
                                        locale.code(),
                                        strings.sys_locale_set
                                    ),
                                    Err(err) => eprintln!("[berimor] не сохранено: {err}"),
                                }
                            }
                            None => eprintln!("[berimor] {arg} — {}", strings.sys_locale_unknown),
                        }
                    }
                    continue;
                }
                "skills" => {
                    let workspace =
                        std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
                    let skills = crate::skills::load_all(&workspace);
                    if skills.is_empty() {
                        eprintln!(
                            "[berimor] скиллы не установлены — berimor skill list --available"
                        );
                    }
                    for skill in &skills {
                        eprintln!(
                            "[berimor] {} v{} — {} [{}]",
                            skill.name,
                            skill.version,
                            skill.description,
                            skill.triggers.join(", ")
                        );
                    }
                    continue;
                }
                "models" => {
                    print_models(config);
                    continue;
                }
                "models add" => {
                    if let Err(err) = setup::run_wizard() {
                        eprintln!("[berimor] мастер настройки: {err}");
                        continue;
                    }
                    return Ok(SessionOutcome::Reload);
                }
                "mouse" | "copy" => {
                    // Мышь в построчном REPL не захватывается вовсе
                    // (выделение и так нативное), буфер обмена
                    // привязан к журналу TUI — честно скажем об этом.
                    eprintln!("[berimor] /{command} — команда полноэкранного TUI; здесь мышь не захватывается");
                    continue;
                }
                _ => {
                    // Slash-триггер скилла: неизвестная встроенная команда
                    // может быть триггером — тогда это сообщение агенту.
                    match resolve_skill_trigger(&chat_skills, message) {
                        Some((augmented, ceiling, name)) => {
                            eprintln!(
                                "[berimor] скилл «{name}» активен (триггер /{})",
                                command.split_whitespace().next().unwrap_or(command)
                            );
                            (augmented, ceiling)
                        }
                        None => {
                            eprintln!("[berimor] неизвестная команда /{command} — /help");
                            continue;
                        }
                    }
                }
            }
        } else {
            // Триггер фразы — кодом (§20.16).
            match resolve_skill_trigger(&chat_skills, message) {
                Some((augmented, ceiling, name)) => {
                    eprintln!("[berimor] скилл «{name}» активен (триггер фразы)");
                    (augmented, ceiling)
                }
                None => (message.to_string(), None),
            }
        };

        let state = json!({
            "goal": turn.0,
            "history": *history,
            "tools": catalog,
        });
        let spinner = chat_ui::Spinner::start(&theme, "berimor думает…");
        // Потолок скилла — per-turn агент с фильтром диспетча (§20.16).
        let ceiling_dispatch = turn.1.as_deref().map(|allowed| CeilingDispatch {
            inner: &ask_dispatch_repl,
            allowed,
        });
        let outcome = match &ceiling_dispatch {
            Some(ceiling) => {
                let turn_agent = AgentStepExecutor {
                    pool: &bundle.pool,
                    providers: &providers,
                    context: &memory_context,
                    on_attempt: Some(&on_attempt),
                    gate: bundle.gate.as_ref(),
                    mode: config.confirmation_mode,
                    confirmer: bundle.confirmer.as_ref(),
                    dispatch: ceiling,
                    secrets: bundle.masker.as_ref(),
                    on_tool_turn: Some(&on_tool_turn),
                    on_provider_switch: None,
                    // BR-01 + §20.16: при потолке скилла перечень в
                    // промпте фильтруется потолком — модель не зовёт
                    // имена, которые CeilingDispatch отклонит.
                    tool_lines: match &turn.1 {
                        Some(allowed) => agent
                            .tool_lines
                            .iter()
                            .filter(|line| {
                                allowed.iter().any(|name| {
                                    line.starts_with(&format!("- {name} "))
                                        || line.starts_with(&format!("- {name}{{"))
                                })
                            })
                            .cloned()
                            .collect(),
                        None => agent.tool_lines.clone(),
                    },
                };
                turn_agent.execute(
                    "chat",
                    ChatReply::NAME,
                    max_turns_per_message(config),
                    false,
                    false,
                    &state,
                    None,
                )
            }
            None => agent.execute(
                "chat",
                ChatReply::NAME,
                max_turns_per_message(config),
                false,
                false,
                &state,
                None,
            ),
        };
        drop(spinner);
        match outcome {
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
                let rendered = chat_ui::render_markdown(&theme, &reply);
                println!(
                    "{}
{}",
                    theme.cyan(&theme.bold("berimor")),
                    rendered
                );
                println!();
                history.push(json!({"role": "user", "content": message}));
                history.push(json!({"role": "assistant", "content": reply.clone()}));
                let workspace = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
                crate::chat_history::append(&workspace, message, &reply);

                // Компакция (prompt-next-wave.md задача 4): если лента
                // разрослась — старая часть сжимается моделью вместо
                // молчаливого посимвольного усечения `apply_budget`.
                let summarize = |old: &[Value]| -> Result<String, String> {
                    let llm = StructuredLlm {
                        pool: &bundle.pool,
                        providers: &providers,
                        context: &memory_context,
                        on_attempt: Some(&on_attempt),
                        secrets: bundle.masker.as_ref(),
                    };
                    let summary_state = json!({"history": old});
                    let patch = llm
                        .execute(
                            "compact_history",
                            HistorySummary::NAME,
                            ModelTierRequirement::Weak,
                            &summary_state,
                            None,
                        )
                        .map_err(|err| err.to_string())?;
                    patch
                        .changes
                        .get("summary")
                        .and_then(Value::as_str)
                        .map(String::from)
                        .ok_or_else(|| "ответ модели не содержит поле summary".to_string())
                };
                match crate::chat_compaction::compact_if_needed(history, &summarize) {
                    Ok(true) => eprintln!("[berimor] лента чата сжата (предыстория суммирована)"),
                    Ok(false) => {}
                    Err(err) => eprintln!(
                        "[berimor] чат: суммаризация предыстории не удалась, лента не сжата: {err}"
                    ),
                }
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

#[cfg(test)]
mod swarm_note_tests {
    use super::*;

    /// §20.22 v3: заметка модели несёт путь/сессию/операцию и роль system.
    #[test]
    fn file_changed_note_carries_context() {
        let envelope = berimor_storage::Envelope {
            id: berimor_storage::EnvelopeId("e-1".into()),
            from: "sess-b".into(),
            to: "sess-a".into(),
            topic: crate::sessions::TOPIC_FILE_CHANGED.into(),
            payload: json!({"path": "src/main.rs", "by_session": "sess-b", "op": "files.write"}),
        };
        let note = file_changed_note(&envelope);
        assert_eq!(note["role"], "system");
        let content = note["content"].as_str().unwrap();
        assert!(content.contains("src/main.rs"));
        assert!(content.contains("sess-b"));
        assert!(content.contains("files.read"));
    }
}
