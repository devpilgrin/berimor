//! Полноэкранный TUI чата (§20.14) — ratatui + crossterm. Директива
//! пользователя: «не хуже Hermes и Claude Code CLI»: фиксированная
//! шапка (не сдвигается журналом), прокручиваемый журнал диалога,
//! поле ввода с историей, строка подсказок, автодополнение
//! slash-команд, модальные меню выбора (пресеты, модели провайдера).
//!
//! Архитектурные решения:
//! - Рантайм агента строится В ВОРКЕР-ПОТОКЕ на каждый ход из
//!   принадлежащей потоку копии конфига — UI-цикл никогда не блокируется
//!   на модели (спиннер живой, ввод отзывчив), а «перезагрузка» после
//!   /models add или /model — просто обновление `App.config`, без
//!   какой-либо пересборки в UI-потоке.
//! - События воркера (ходы инструментов, финальный ответ) — через
//!   mpsc-канал; UI единственный писатель в журнал (нет гонок).
//! - Не-терминал (пайпы, e2e) — старый построчный REPL (chat.rs):
//!   TUI включается только на настоящем терминале.
//! - Выбор модели — ЖИВОЙ список `GET {base_url}/models` провайдера
//!   (OpenAI-совместимый): пресет задаёт endpoint и умолчание, не
//!   запирает на одной модели (замечание пользователя §20.14).

use crate::config::{self, Config, ProviderConfig};
use crate::presets::{self, ProviderPreset};
use crate::run::RunError;
use crate::setup;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::{Frame, Terminal};
use serde_json::Value;
use std::io::Stdout;
use std::path::Path;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::Duration;

/// Slash-команды с описаниями — источник и для автодополнения, и для
/// /help (одно определение, два потребителя).
const SLASH_COMMANDS: &[(&str, &str)] = &[
    ("/help", "список команд"),
    ("/config", "эффективная конфигурация"),
    ("/models", "провайдеры моделей"),
    (
        "/models add",
        "мастер: пресеты → живой список моделей → ключ",
    ),
    (
        "/model",
        "сменить модель сессии (выбор из списка провайдера)",
    ),
    ("/exit", "завершить"),
    ("/quit", "завершить"),
];

const SPINNER: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

/// Событие от воркер-потока агента в UI-цикл.
pub(crate) enum WorkerMsg {
    ToolTurn(String),
    Reply(Result<String, String>),
}

/// Строка журнала диалога.
enum LogLine {
    User(String),
    Assistant(String),
    Tool(String),
    Sys(String),
    Err(String),
}

/// Модальное меню выбора (стрелки/Enter; multi — Space помечает).
struct Picker {
    title: String,
    items: Vec<String>,
    state: ListState,
    multi: bool,
    marked: Vec<bool>,
}

impl Picker {
    fn new(title: impl Into<String>, items: Vec<String>, multi: bool) -> Self {
        let mut state = ListState::default();
        state.select(Some(0));
        Self {
            title: title.into(),
            marked: vec![false; items.len()],
            state,
            items,
            multi,
        }
    }

    fn move_by(&mut self, delta: isize) {
        if self.items.is_empty() {
            return;
        }
        let cur = self.state.selected().unwrap_or(0) as isize;
        let next = (cur + delta).rem_euclid(self.items.len() as isize) as usize;
        self.state.select(Some(next));
    }

    fn toggle(&mut self) {
        if let Some(i) = self.state.selected() {
            if i < self.marked.len() {
                self.marked[i] = !self.marked[i];
            }
        }
    }

    fn selected(&self) -> Option<usize> {
        self.state.selected()
    }

    fn marked_indexes(&self) -> Vec<usize> {
        self.marked
            .iter()
            .enumerate()
            .filter(|(_, m)| **m)
            .map(|(i, _)| i)
            .collect()
    }
}

/// Многошаговый сценарий модальных диалогов.
enum Flow {
    /// /models add: выбор пресетов (multi).
    AddPickPresets,
    /// /models add: выбор модели для пресета (из живого списка).
    AddPickModel {
        preset: &'static ProviderPreset,
        models: Vec<String>,
    },
    /// /models add: ввод ключа (маскируется звёздочками).
    AddAskKey {
        provider: ProviderConfig,
        key_env: String,
        input: String,
    },
    /// /model: выбор провайдера из эффективного конфига.
    SwitchPickProvider,
    /// /model: выбор модели провайдера.
    SwitchPickModel { provider_name: String },
}

struct App {
    config: Config,
    explicit_config: Option<std::path::PathBuf>,
    log: Vec<LogLine>,
    input: String,
    cursor: usize,
    history: Vec<String>,
    history_idx: Option<usize>,
    /// Лента диалога для состояния агента (role/content).
    conversation: Vec<Value>,
    scroll: u16,
    follow_tail: bool,
    busy: bool,
    spinner_frame: usize,
    slash_open: bool,
    slash_state: ListState,
    picker: Option<Picker>,
    flow: Option<Flow>,
    /// Очередь пресетов, ожидающих выбора модели/ключа (после multi-пикера).
    pending_presets: Vec<&'static ProviderPreset>,
    /// Накопленные провайдеры и ключи текущего /models add.
    staging_providers: Vec<ProviderConfig>,
    staging_keys: Vec<(String, String)>,
    tx: Sender<WorkerMsg>,
    rx: Receiver<WorkerMsg>,
    done: bool,
}

/// Живой список моделей провайдера (OpenAI-совместимый GET /models).
/// Ошибка — не смерть сценария: вызывающий код откатывается на
/// умолчание пресета (список из одного пункта).
pub fn fetch_models(provider: &ProviderConfig) -> Result<Vec<String>, String> {
    let url = format!("{}/models", provider.base_url.trim_end_matches('/'));
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|e| e.to_string())?;
    let mut request = client.get(&url);
    if let Some(env_name) = &provider.api_key_env {
        if let Ok(key) = std::env::var(env_name) {
            request = request.bearer_auth(key);
        }
    }
    let response = request.send().map_err(|e| format!("{url}: {e}"))?;
    let body: Value = response.json().map_err(|e| e.to_string())?;
    let mut models: Vec<String> = body
        .get("data")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|m| m.get("id").and_then(Value::as_str).map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    models.sort();
    Ok(models)
}

pub fn run_tui(explicit_config: Option<&Path>) -> Result<(), RunError> {
    let config =
        config::load(explicit_config).map_err(|err| RunError::BadInput(err.to_string()))?;
    let (tx, rx) = channel();
    let mut app = App {
        config,
        explicit_config: explicit_config.map(Path::to_path_buf),
        log: Vec::new(),
        input: String::new(),
        cursor: 0,
        history: Vec::new(),
        history_idx: None,
        conversation: Vec::new(),
        scroll: 0,
        follow_tail: true,
        busy: false,
        spinner_frame: 0,
        slash_open: false,
        slash_state: ListState::default(),
        picker: None,
        flow: None,
        pending_presets: Vec::new(),
        staging_providers: Vec::new(),
        staging_keys: Vec::new(),
        tx,
        rx,
        done: false,
    };
    app.sys("berimor chat — /help или / для команд");
    if app.config.providers.is_empty() {
        app.sys("провайдеры не настроены — /models add");
    }

    let mut terminal = TerminalGuard::new().map_err(|e| RunError::BadInput(e.to_string()))?;
    let result = event_loop(&mut app, &mut terminal.0);
    drop(terminal); // восстановление терминала — до вывода ошибки
    result
}

/// RAII: raw mode + alternate screen, восстановление при выходе
/// (включая панику выше по стеку — терминал пользователя святость).
struct TerminalGuard(Terminal<ratatui::backend::CrosstermBackend<Stdout>>);

impl TerminalGuard {
    fn new() -> std::io::Result<Self> {
        enable_raw_mode()?;
        let mut stdout = std::io::stdout();
        stdout.execute(EnterAlternateScreen)?;
        let backend = ratatui::backend::CrosstermBackend::new(stdout);
        Ok(Self(Terminal::new(backend)?))
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = self.0.backend_mut().execute(LeaveAlternateScreen);
        let _ = self.0.show_cursor();
    }
}

fn event_loop(
    app: &mut App,
    terminal: &mut Terminal<ratatui::backend::CrosstermBackend<Stdout>>,
) -> Result<(), RunError> {
    while !app.done {
        terminal
            .draw(|frame| draw(frame, app))
            .map_err(|e| RunError::BadInput(e.to_string()))?;

        // События воркера — без ожидания клавиш.
        while let Ok(msg) = app.rx.try_recv() {
            match msg {
                WorkerMsg::ToolTurn(text) => {
                    app.log.push(LogLine::Tool(text));
                    app.maybe_follow();
                }
                WorkerMsg::Reply(Ok(reply)) => {
                    app.busy = false;
                    app.conversation
                        .push(serde_json::json!({"role": "assistant", "content": reply}));
                    app.log.push(LogLine::Assistant(reply));
                    app.maybe_follow();
                }
                WorkerMsg::Reply(Err(err)) => {
                    app.busy = false;
                    app.log.push(LogLine::Err(err));
                    app.maybe_follow();
                }
            }
        }

        if event::poll(Duration::from_millis(80)).map_err(|e| RunError::BadInput(e.to_string()))? {
            if let Event::Key(key) = event::read().map_err(|e| RunError::BadInput(e.to_string()))? {
                handle_key(app, key);
            }
        } else if app.busy {
            app.spinner_frame += 1; // тик спиннера по таймауту poll
        }
    }
    Ok(())
}

impl App {
    fn sys(&mut self, text: impl Into<String>) {
        self.log.push(LogLine::Sys(text.into()));
        self.maybe_follow();
    }

    fn maybe_follow(&mut self) {
        if self.follow_tail {
            self.scroll = u16::MAX; // при рендере прижмётся к низу
        }
    }

    fn slash_filtered(&self) -> Vec<&'static (&'static str, &'static str)> {
        let typed = self.input.trim_end();
        SLASH_COMMANDS
            .iter()
            .filter(|(name, _)| name.starts_with(typed) || typed == "/")
            .collect()
    }

    fn open_slash(&mut self) {
        self.slash_open = true;
        self.slash_state.select(Some(0));
    }

    fn apply_slash_completion(&mut self) {
        let filtered = self.slash_filtered();
        if let Some(i) = self.slash_state.selected() {
            if let Some((name, _)) = filtered.get(i) {
                self.input = name.to_string();
                self.cursor = self.input.len();
                if !name.contains(' ') {
                    self.input.push(' ');
                    self.cursor += 1;
                }
            }
        }
        self.slash_open = false;
    }

    fn submit(&mut self) {
        let message = self.input.trim().to_string();
        if message.is_empty() {
            return;
        }
        self.history.push(message.clone());
        self.history_idx = None;
        self.input.clear();
        self.cursor = 0;
        self.slash_open = false;

        if let Some(command) = message.strip_prefix('/') {
            self.run_command(command);
            return;
        }
        self.log.push(LogLine::User(message.clone()));
        self.conversation
            .push(serde_json::json!({"role": "user", "content": message.clone()}));
        self.maybe_follow();
        self.start_turn(message);
    }

    /// Ход агента — в воркер-потоке с собственным рантаймом (UI не
    /// блокируется; конфиг клонируется — «перезагрузка» бесплатна).
    fn start_turn(&self, message: String) {
        let config = self.config.clone();
        let conversation = self.conversation.clone();
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let reply = crate::chat::execute_turn(&config, conversation, message, tx.clone());
            let _ = tx.send(WorkerMsg::Reply(reply));
        });
    }

    fn run_command(&mut self, command: &str) {
        match command {
            "exit" | "quit" => self.done = true,
            "help" => {
                for (name, about) in SLASH_COMMANDS {
                    self.sys(format!("{name:<12} — {about}"));
                }
            }
            "config" => {
                self.sys(format!("журнал: {}", self.config.storage_path.display()));
                self.sys(format!(
                    "режим подтверждений: {:?}",
                    self.config.confirmation_mode
                ));
                self.sys(format!("провайдеров: {}", self.config.providers.len()));
            }
            "models" => {
                if self.config.providers.is_empty() {
                    self.sys("провайдеры не настроены — /models add");
                }
                let lines: Vec<String> = self
                    .config
                    .providers
                    .iter()
                    .map(|p| format!("{} — {} ({:?})", p.name, p.model_id, p.tier))
                    .collect();
                for line in lines {
                    self.sys(line);
                }
            }
            "models add" => {
                let items: Vec<String> = presets::PRESETS
                    .iter()
                    .map(|p| format!("{} — {}", p.display, p.about))
                    .collect();
                self.picker = Some(Picker::new(
                    "Пресеты (Space — пометить, Enter — подтвердить)",
                    items,
                    true,
                ));
                self.flow = Some(Flow::AddPickPresets);
            }
            "model" => {
                if self.config.providers.is_empty() {
                    self.sys("провайдеры не настроены — /models add");
                    return;
                }
                let items: Vec<String> = self
                    .config
                    .providers
                    .iter()
                    .map(|p| format!("{} — {}", p.name, p.model_id))
                    .collect();
                self.picker = Some(Picker::new("Провайдер (Enter — выбрать)", items, false));
                self.flow = Some(Flow::SwitchPickProvider);
            }
            other => self.sys(format!("неизвестная команда /{other} — /help")),
        }
    }

    /// Продвижение многошагового сценария после подтверждения пикера.
    fn advance_flow(&mut self) {
        let Some(flow) = self.flow.take() else { return };
        let Some(picker) = self.picker.take() else {
            return;
        };
        match flow {
            Flow::AddPickPresets => {
                let marked = picker.marked_indexes();
                let chosen: Vec<&'static ProviderPreset> = if marked.is_empty() {
                    picker
                        .selected()
                        .and_then(|i| presets::PRESETS.get(i))
                        .into_iter()
                        .collect()
                } else {
                    marked
                        .iter()
                        .filter_map(|i| presets::PRESETS.get(*i))
                        .collect()
                };
                self.pending_presets = chosen;
                self.staging_providers.clear();
                self.staging_keys.clear();
                self.next_preset();
            }
            Flow::AddPickModel { preset, models } => {
                let model = picker
                    .selected()
                    .and_then(|i| models.get(i))
                    .cloned()
                    .unwrap_or_else(|| preset.default_model.to_string());
                let provider = presets::instantiate(preset, Some(model), None);
                if let Some(env_name) = preset.key_env {
                    if std::env::var_os(env_name).is_none() {
                        self.flow = Some(Flow::AddAskKey {
                            provider,
                            key_env: env_name.to_string(),
                            input: String::new(),
                        });
                        return;
                    }
                }
                self.staging_providers.push(provider);
                self.next_preset();
            }
            Flow::SwitchPickProvider => {
                let Some(index) = picker.selected() else {
                    return;
                };
                let Some(provider) = self.config.providers.get(index).cloned() else {
                    return;
                };
                let models = fetch_models(&provider).unwrap_or_else(|err| {
                    self.sys(format!(
                        "список моделей недоступен ({err}) — текущая модель"
                    ));
                    vec![provider.model_id.clone()]
                });
                let items = models.clone();
                self.picker = Some(Picker::new(
                    format!("Модель {} (Enter — выбрать)", provider.name),
                    items,
                    false,
                ));
                self.flow = Some(Flow::SwitchPickModel {
                    provider_name: provider.name,
                });
                // stash models in picker items; flow carries provider
                let _ = models;
            }
            Flow::SwitchPickModel { provider_name } => {
                let Some(index) = picker.selected() else {
                    return;
                };
                let model = picker.items[index].clone();
                if let Some(provider) = self
                    .config
                    .providers
                    .iter_mut()
                    .find(|p| p.name == provider_name)
                {
                    provider.model_id = model.clone();
                }
                self.sys(format!(
                    "модель сессии: {provider_name} → {model} (до конца сессии; закрепить — model_id в конфиге)"
                ));
            }
            Flow::AddAskKey { .. } => {} // обрабатывается в вводе, не пикером
        }
    }

    /// Следующий пресет из очереди /models add: живой список моделей →
    /// пикер; очередь пуста — запись в глобальный конфиг.
    fn next_preset(&mut self) {
        let Some(preset) = self.pending_presets.first().copied() else {
            self.finish_add();
            return;
        };
        self.pending_presets.remove(0);
        let probe = presets::instantiate(preset, None, None);
        let models = fetch_models(&probe).unwrap_or_else(|err| {
            self.sys(format!(
                "{}: список моделей недоступен ({err}) — умолчание пресета",
                preset.display
            ));
            vec![preset.default_model.to_string()]
        });
        self.picker = Some(Picker::new(
            format!("Модель для {} (Enter — выбрать)", preset.display),
            models.clone(),
            false,
        ));
        self.flow = Some(Flow::AddPickModel { preset, models });
    }

    fn finish_add(&mut self) {
        let Some(global) = config::global_config_path() else {
            self.sys("нет глобальной директории (HOME/XDG) — запись невозможна");
            return;
        };
        let Some(secrets) = config::secrets_env_path() else {
            return;
        };
        match setup::append_providers(&global, &self.staging_providers) {
            Ok(added) => {
                if added.is_empty() {
                    self.sys("новых провайдеров нет (имена уже заняты)");
                } else {
                    self.sys(format!("добавлены: {}", added.join(", ")));
                }
            }
            Err(err) => {
                self.sys(format!("запись конфига: {err}"));
                return;
            }
        }
        let mut keys = 0;
        for (name, value) in &self.staging_keys {
            if let Ok(true) = setup::append_secret(&secrets, name, value) {
                keys += 1;
            }
        }
        if keys > 0 {
            self.sys(format!("ключи записаны в {} (0600)", secrets.display()));
        }
        // Перезагрузка эффективного конфига из файлов.
        match config::load(self.explicit_config.as_deref()) {
            Ok(fresh) => {
                self.config = fresh;
                self.sys("конфигурация перечитана");
            }
            Err(err) => self.sys(format!("перечитать конфиг: {err}")),
        }
    }
}

fn handle_key(app: &mut App, key: KeyEvent) {
    if key.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('d'))
    {
        app.done = true;
        return;
    }

    // Ввод ключа API (маскируемый) — отдельный режим.
    if let Some(Flow::AddAskKey { input, .. }) = &mut app.flow {
        match key.code {
            KeyCode::Enter => {
                if let Some(Flow::AddAskKey {
                    provider,
                    key_env,
                    input,
                }) = app.flow.take()
                {
                    if !input.is_empty() {
                        app.staging_keys.push((key_env, input));
                    }
                    app.staging_providers.push(provider);
                    app.next_preset();
                }
            }
            KeyCode::Esc => {
                if let Some(Flow::AddAskKey { provider, .. }) = app.flow.take() {
                    app.staging_providers.push(provider);
                    app.next_preset();
                }
            }
            KeyCode::Backspace => {
                input.pop();
            }
            KeyCode::Char(c) => input.push(c),
            _ => {}
        }
        return;
    }

    // Модальный пикер перехватывает клавиши.
    if app.picker.is_some() {
        match key.code {
            KeyCode::Up => app.picker.as_mut().unwrap().move_by(-1),
            KeyCode::Down => app.picker.as_mut().unwrap().move_by(1),
            KeyCode::Char(' ') => app.picker.as_mut().unwrap().toggle(),
            KeyCode::Enter => app.advance_flow(),
            KeyCode::Esc => {
                app.picker = None;
                app.flow = None;
                app.pending_presets.clear();
            }
            _ => {}
        }
        return;
    }

    match key.code {
        KeyCode::Enter => app.submit(),
        KeyCode::Esc => app.slash_open = false,
        KeyCode::Tab if app.slash_open => app.apply_slash_completion(),
        KeyCode::Up if app.slash_open => {
            let len = app.slash_filtered().len();
            if len > 0 {
                let cur = app.slash_state.selected().unwrap_or(0);
                app.slash_state.select(Some((cur + len - 1) % len));
            }
        }
        KeyCode::Down if app.slash_open => {
            let len = app.slash_filtered().len();
            if len > 0 {
                let cur = app.slash_state.selected().unwrap_or(0);
                app.slash_state.select(Some((cur + 1) % len));
            }
        }
        KeyCode::Up => {
            if !app.history.is_empty() {
                let idx = app
                    .history_idx
                    .map(|i| i.saturating_sub(1))
                    .unwrap_or(app.history.len() - 1);
                app.history_idx = Some(idx);
                app.input = app.history[idx].clone();
                app.cursor = app.input.len();
            }
        }
        KeyCode::Down => {
            if let Some(idx) = app.history_idx {
                if idx + 1 < app.history.len() {
                    app.history_idx = Some(idx + 1);
                    app.input = app.history[idx + 1].clone();
                } else {
                    app.history_idx = None;
                    app.input.clear();
                }
                app.cursor = app.input.len();
            }
        }
        KeyCode::PageUp => {
            app.follow_tail = false;
            app.scroll = app.scroll.saturating_sub(10).min(u16::MAX - 1);
            if app.scroll == u16::MAX {
                app.scroll = 0;
            }
        }
        KeyCode::PageDown => {
            app.scroll = app.scroll.saturating_add(10);
            app.follow_tail = false;
        }
        KeyCode::Left => {
            // Курсор хранится в БАЙТАХ (границах символов): insert/remove
            // в String — байтовые. Движение — по символам через
            // char_indices, иначе первая же кириллица (2 байта) давала
            // панику «not a char boundary» (репорт 2026-08-03).
            app.cursor = app.input[..app.cursor]
                .char_indices()
                .next_back()
                .map(|(i, _)| i)
                .unwrap_or(0);
        }
        KeyCode::Right => {
            app.cursor = app.input[app.cursor..]
                .char_indices()
                .nth(1)
                .map(|(i, _)| app.cursor + i)
                .unwrap_or(app.input.len());
        }
        KeyCode::Home => app.cursor = 0,
        KeyCode::End => app.cursor = app.input.len(),
        KeyCode::Backspace => {
            if app.cursor > 0 {
                let prev = app.input[..app.cursor]
                    .chars()
                    .next_back()
                    .map(char::len_utf8)
                    .unwrap_or(0);
                app.cursor -= prev;
                app.input.remove(app.cursor);
                if app.input.is_empty() || !app.input.starts_with('/') {
                    app.slash_open = false;
                }
            }
        }
        KeyCode::Delete => {
            if app.cursor < app.input.len() {
                app.input.remove(app.cursor);
            }
        }
        KeyCode::Char(c) => {
            app.input.insert(app.cursor, c);
            app.cursor += c.len_utf8();
            if app.input == "/" {
                app.open_slash();
            } else if app.slash_open && !app.input.starts_with('/') {
                app.slash_open = false;
            }
        }
        _ => {}
    }
}

fn draw(frame: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4), // шапка — фиксированная, не сдвигаемая
            Constraint::Min(3),    // журнал
            Constraint::Length(3), // ввод
            Constraint::Length(1), // подсказки
        ])
        .split(frame.area());
    draw_header(frame, app, chunks[0]);
    draw_log(frame, app, chunks[1]);
    draw_input(frame, app, chunks[2]);
    draw_hints(frame, app, chunks[3]);
    if app.slash_open {
        draw_slash_popup(frame, app, chunks[2]);
    }
    if let Some(picker) = &app.picker {
        draw_picker(frame, picker);
    }
    if let Some(Flow::AddAskKey { key_env, input, .. }) = &app.flow {
        draw_key_prompt(frame, key_env, input.len());
    }
}

fn draw_header(frame: &mut Frame, app: &App, area: Rect) {
    let workspace = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| ".".into());
    let models: Vec<String> = app
        .config
        .providers
        .iter()
        .map(|p| format!("{}:{}", p.name, p.model_id))
        .collect();
    let spinner = if app.busy {
        format!(" {} думаю…", SPINNER[app.spinner_frame % SPINNER.len()])
    } else {
        String::new()
    };
    let header = Paragraph::new(vec![
        Line::from(vec![
            Span::styled(
                format!(" berimor v{}", env!("CARGO_PKG_VERSION")),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(spinner, Style::default().fg(Color::Magenta)),
        ]),
        Line::from(vec![
            Span::styled(" область: ", Style::default().fg(Color::DarkGray)),
            Span::raw(workspace),
        ]),
        Line::from(vec![
            Span::styled(" модели: ", Style::default().fg(Color::DarkGray)),
            Span::raw(if models.is_empty() {
                "не настроены — /models add".to_string()
            } else {
                models.join("  ")
            }),
        ]),
    ])
    .block(
        Block::default()
            .borders(Borders::BOTTOM)
            .border_style(Style::default().fg(Color::DarkGray)),
    );
    frame.render_widget(header, area);
}

/// Markdown-lite → ratatui Lines: fenced-блоки (dim), заголовки (cyan),
/// **полужирный**, `код` (yellow).
fn md_lines(text: &str) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let mut in_code = false;
    for raw in text.lines() {
        if raw.trim_start().starts_with("```") {
            in_code = !in_code;
            continue;
        }
        if in_code {
            lines.push(Line::from(Span::styled(
                format!("  {raw}"),
                Style::default().fg(Color::DarkGray),
            )));
            continue;
        }
        if let Some(header) = raw.strip_prefix("## ").or_else(|| raw.strip_prefix("# ")) {
            lines.push(Line::from(Span::styled(
                header.to_string(),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )));
            continue;
        }
        lines.push(inline_spans(raw));
    }
    lines
}

fn inline_spans(text: &str) -> Line<'static> {
    let mut spans = Vec::new();
    let mut rest = text.to_string();
    loop {
        let bold_pos = rest.find("**");
        let code_pos = rest.find('`');
        let (pos, marker_len, style) = match (bold_pos, code_pos) {
            (Some(b), Some(c)) if b <= c => (b, 2, Style::default().add_modifier(Modifier::BOLD)),
            (Some(b), Some(c)) if c < b => (c, 1, Style::default().fg(Color::Yellow)),
            (Some(b), None) => (b, 2, Style::default().add_modifier(Modifier::BOLD)),
            (None, Some(c)) => (c, 1, Style::default().fg(Color::Yellow)),
            (None, None) => {
                spans.push(Span::raw(rest));
                return Line::from(spans);
            }
            _ => unreachable!(),
        };
        spans.push(Span::raw(rest[..pos].to_string()));
        let after = rest[pos + marker_len..].to_string();
        let close = if marker_len == 2 {
            after.find("**")
        } else {
            after.find('`')
        };
        match close {
            Some(end) => {
                spans.push(Span::styled(after[..end].to_string(), style));
                rest = after[end + marker_len..].to_string();
            }
            None => {
                spans.push(Span::raw(rest[pos..].to_string()));
                return Line::from(spans);
            }
        }
    }
}

fn draw_log(frame: &mut Frame, app: &App, area: Rect) {
    let mut lines: Vec<Line> = Vec::new();
    for entry in &app.log {
        match entry {
            LogLine::User(text) => {
                lines.push(Line::from(""));
                lines.push(Line::from(vec![
                    Span::styled(
                        " › ",
                        Style::default()
                            .fg(Color::Green)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(text.clone(), Style::default().add_modifier(Modifier::BOLD)),
                ]));
            }
            LogLine::Assistant(text) => {
                lines.push(Line::from(Span::styled(
                    " berimor",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                )));
                lines.extend(md_lines(text));
            }
            LogLine::Tool(text) => lines.push(Line::from(Span::styled(
                format!("   {text}"),
                Style::default().fg(Color::DarkGray),
            ))),
            LogLine::Sys(text) => lines.push(Line::from(Span::styled(
                format!(" · {text}"),
                Style::default().fg(Color::Magenta),
            ))),
            LogLine::Err(text) => lines.push(Line::from(Span::styled(
                format!(" ! {text}"),
                Style::default().fg(Color::Red),
            ))),
        }
    }
    let height = area.height.saturating_sub(2) as usize;
    let total = lines.len();
    let max_scroll = total.saturating_sub(height);
    let scroll = if app.scroll == u16::MAX || app.follow_tail {
        max_scroll
    } else {
        (app.scroll as usize).min(max_scroll)
    };
    let visible: Vec<Line> = lines.into_iter().skip(scroll).collect();
    let log = Paragraph::new(visible).wrap(Wrap { trim: false });
    frame.render_widget(log, area);
}

fn draw_input(frame: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(Style::default().fg(Color::DarkGray));
    let prompt = Paragraph::new(Line::from(vec![
        Span::styled(
            " › ",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(app.input.clone()),
    ]))
    .block(block);
    frame.render_widget(prompt, area);
    // Курсор на экране — в СИМВОЛАХ (кириллица — 1 колонка, 2 байта).
    let display_cursor = app.input[..app.cursor].chars().count() as u16;
    frame.set_cursor_position((area.x + 3 + display_cursor, area.y + 1));
}

fn draw_hints(frame: &mut Frame, app: &App, area: Rect) {
    let hints = if app.busy {
        " агент работает… · Ctrl+C — выход"
    } else if app.picker.is_some() {
        " ↑↓ — выбор · Space — пометить · Enter — подтвердить · Esc — отмена"
    } else if app.slash_open {
        " ↑↓ — выбор · Tab/Enter — вставить · Esc — закрыть"
    } else {
        " / — команды · Enter — отправить · PgUp/PgDn — журнал · ↑↓ — история · /exit — выход"
    };
    frame.render_widget(
        Paragraph::new(Span::styled(hints, Style::default().fg(Color::DarkGray))),
        area,
    );
}

fn draw_slash_popup(frame: &mut Frame, app: &App, input_area: Rect) {
    let filtered = app.slash_filtered();
    let height = (filtered.len() as u16 + 2).min(10);
    let area = Rect {
        x: input_area.x + 2,
        y: input_area.y.saturating_sub(height),
        width: 60.min(input_area.width.saturating_sub(4)),
        height,
    };
    let items: Vec<ListItem> = filtered
        .iter()
        .map(|(name, about)| ListItem::new(format!("{name:<12} {about}")))
        .collect();
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title("команды"))
        .highlight_style(Style::default().bg(Color::DarkGray));
    let mut state = app.slash_state.clone();
    frame.render_widget(Clear, area);
    frame.render_stateful_widget(list, area, &mut state);
}

fn draw_picker(frame: &mut Frame, picker: &Picker) {
    let area = centered_rect(70, 60, frame.area());
    let items: Vec<ListItem> = picker
        .items
        .iter()
        .enumerate()
        .map(|(i, item)| {
            let mark = if picker.multi {
                if picker.marked[i] {
                    "[x] "
                } else {
                    "[ ] "
                }
            } else {
                ""
            };
            ListItem::new(format!("{mark}{item}"))
        })
        .collect();
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" {} ", picker.title)),
        )
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        );
    let mut state = picker.state.clone();
    frame.render_widget(Clear, area);
    frame.render_stateful_widget(list, area, &mut state);
}

fn draw_key_prompt(frame: &mut Frame, key_env: &str, input_len: usize) {
    let area = centered_rect(60, 20, frame.area());
    let prompt = Paragraph::new(vec![
        Line::from(format!(" Ключ API ({key_env}):")),
        Line::from(format!(" {}", "*".repeat(input_len))),
        Line::from(Span::styled(
            " Enter — сохранить · Esc — пропустить",
            Style::default().fg(Color::DarkGray),
        )),
    ])
    .block(Block::default().borders(Borders::ALL).title(" секрет "));
    frame.render_widget(Clear, area);
    frame.render_widget(prompt, area);
}

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1])[1]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slash_filter_prefixes() {
        let config = Config::default();
        let (tx, rx) = channel();
        let app = App {
            config,
            explicit_config: None,
            log: vec![],
            input: "/mod".into(),
            cursor: 4,
            history: vec![],
            history_idx: None,
            conversation: vec![],
            scroll: 0,
            follow_tail: true,
            busy: false,
            spinner_frame: 0,
            slash_open: true,
            slash_state: ListState::default(),
            picker: None,
            flow: None,
            pending_presets: vec![],
            staging_providers: vec![],
            staging_keys: vec![],
            tx,
            rx,
            done: false,
        };
        let names: Vec<&str> = app.slash_filtered().iter().map(|(n, _)| *n).collect();
        assert!(names.contains(&"/model"));
        assert!(names.contains(&"/models"));
        assert!(!names.contains(&"/help"));
    }

    #[test]
    fn picker_multi_marks() {
        let mut picker = Picker::new("t", vec!["a".into(), "b".into(), "c".into()], true);
        picker.toggle();
        picker.move_by(2);
        picker.toggle();
        assert_eq!(picker.marked_indexes(), vec![0, 2]);
    }

    #[test]
    fn md_lines_handle_fences_and_headers() {
        let lines = md_lines("## Заголовок\nтекст\n```\ncode\n```");
        assert_eq!(lines.len(), 3);
        let plain: String = lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.to_string())
                    .collect::<String>()
            })
            .collect();
        assert!(plain.contains("Заголовок"));
        assert!(plain.contains("code"));
        assert!(!plain.contains("```"));
    }

    #[test]
    fn cyrillic_input_never_panics_on_char_boundaries() {
        // Репорт 2026-08-03: ввод в TUI вылетал — курсор в символах против
        // байтовых insert/remove. Гоняем реальный handle_key.
        let config = Config::default();
        let (tx, rx) = channel();
        let mut app = App {
            config,
            explicit_config: None,
            log: vec![],
            input: String::new(),
            cursor: 0,
            history: vec![],
            history_idx: None,
            conversation: vec![],
            scroll: 0,
            follow_tail: true,
            busy: false,
            spinner_frame: 0,
            slash_open: false,
            slash_state: ListState::default(),
            picker: None,
            flow: None,
            pending_presets: vec![],
            staging_providers: vec![],
            staging_keys: vec![],
            tx,
            rx,
            done: false,
        };
        let key = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
        for c in "привет".chars() {
            handle_key(&mut app, key(c));
        }
        assert_eq!(app.input, "привет");
        // Влево два символа, вставка латиницы посередине, backspace.
        handle_key(&mut app, KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        handle_key(&mut app, KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        handle_key(&mut app, key('X'));
        assert_eq!(app.input, "привXет");
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE),
        );
        assert_eq!(app.input, "привет");
        handle_key(&mut app, KeyEvent::new(KeyCode::Home, KeyModifiers::NONE));
        handle_key(&mut app, KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE));
        assert_eq!(app.input, "ривет");
    }

    #[test]
    fn inline_spans_unclosed_marker_stays_literal() {
        let line = inline_spans("звёздочки ** не маркер");
        let text: String = line.spans.iter().map(|s| s.content.to_string()).collect();
        assert!(text.contains("** не маркер"));
    }
}
