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
use crate::i18n::{self, Locale};
use crate::presets::{self, ProviderPreset};
use crate::run::RunError;
use crate::setup;
use crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event, KeyCode, KeyEvent, KeyModifiers, MouseEvent,
};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Clear, List, ListItem, ListState, Paragraph, Scrollbar, ScrollbarOrientation,
    ScrollbarState, Wrap,
};
use ratatui::{Frame, Terminal};
use serde_json::Value;
use std::io::Stdout;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::Duration;

/// Slash-команды с описаниями — источник и для автодополнения, и для
/// /help (одно определение, два потребителя).
/// Описание slash-команды на конкретной локали — поле `Strings`.
type SlashAbout = fn(&i18n::Strings) -> &'static str;

/// Slash-команды: имя + описание на локали (i18n, 2026-08-09).
/// Описание — fn-указатель на поле `Strings`: таблица одна на язык,
/// палитра и /help читают её через `i18n::strings(app.locale)`.
const SLASH_COMMANDS: &[(&str, SlashAbout)] = &[
    ("/help", |s| s.slash_help),
    ("/config", |s| s.slash_config),
    ("/config locale", |s| s.slash_config_locale),
    ("/models", |s| s.slash_models),
    ("/models add", |s| s.slash_models_add),
    ("/model", |s| s.slash_model),
    ("/tools", |s| s.slash_tools),
    ("/skills", |s| s.slash_skills),
    ("/skills add", |s| s.slash_skills_add),
    ("/skills remove", |s| s.slash_skills_remove),
    ("/agents", |s| s.slash_agents),
    ("/agents add", |s| s.slash_agents_add),
    ("/agents remove", |s| s.slash_agents_remove),
    ("/plugins", |s| s.slash_plugins),
    ("/plugins add", |s| s.slash_plugins_add),
    ("/plugins remove", |s| s.slash_plugins_remove),
    ("/exit", |s| s.slash_exit),
    ("/quit", |s| s.slash_exit),
    ("/mouse", |s| s.slash_mouse),
    ("/copy", |s| s.slash_copy),
];

const SPINNER: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

/// Ответ модала подтверждения (0.14.0): не «да/нет», а с областью
/// действия — директива «разрешить / разрешить для проекта / разрешить
/// для сессии / нет». Порядок в модале — как здесь.
#[derive(Clone, Copy)]
pub(crate) enum ConfirmAnswer {
    /// Одно действие.
    Once,
    /// Этот инструмент — до конца сессии.
    Session,
    /// Этот инструмент — для области (пишется в `.berimor/allow`).
    Project,
    /// ВСЕ инструменты — для области (`*` в `.berimor/allow`, 0.14.1:
    /// «я давал везде разрешение на проект» — разрешение на РАБОТУ,
    /// не на один инструмент). Deny-статика, jail и external_effect —
    /// выше широкого разрешения.
    ProjectAll,
    /// Отказ.
    Deny,
}

/// Событие от воркер-потока агента в UI-цикл.
pub(crate) enum WorkerMsg {
    ToolTurn(String),
    /// Служебная строка в ленту (failover провайдера и т.п.).
    Sys(String),
    ConfirmRequest(String),
    /// human.ask (B7): вопрос агента к человеку — модал с полем ввода;
    /// ответ возвращается по каналу хода (`ask_answer_tx`).
    AskRequest(String),
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
    /// /models add: ввод ключа (маскируется звёздочками) — ДО выбора
    /// модели: список `/models` у облачных провайдеров требует
    /// авторизации.
    AddAskKey {
        preset: &'static ProviderPreset,
        key_env: String,
        input: String,
    },
    /// /model: выбор провайдера из эффективного конфига.
    SwitchPickProvider,
    /// /model: выбор модели провайдера.
    SwitchPickModel { provider_name: String },
    /// /skills add, /agents add (§20.36): пикер из живого git-каталога
    /// (тот же приём, что AddPickModel — картинка для показа отдельно
    /// от чистого имени для установки). `names[i]` соответствует
    /// `picker.items[i]`.
    ExtCatalogPick {
        kind: crate::ext_cmd::ExtKind,
        names: Vec<String>,
    },
    /// /skills remove, /agents remove: пикер из УСТАНОВЛЕННЫХ.
    ExtRemovePick {
        kind: crate::ext_cmd::ExtKind,
        names: Vec<String>,
    },
    /// /plugins add: ввод URL репозитория (не маскируется — не секрет).
    /// Установка — не пикер (нет каталога плагинов, см. catalog.rs) и
    /// не эта конечная точка: Enter лишь копит `input`, реальный запуск
    /// — через `App.pending_plugin_install`, обработанный в
    /// `event_loop` (нужен доступ к `Terminal` для приостановки TUI).
    PluginAskRepo { input: String },
    /// /plugins remove: пикер из установленных плагинов.
    PluginRemovePick { names: Vec<String> },
    /// /config locale: пикер из 8 локалей (i18n, 2026-08-09).
    LocalePick,
    /// /config: меню параметров (2026-08-12: «/config > модалка с
    /// выбором параметров > Locale > модалка языка»).
    ConfigMenu,
}

/// Фокус мыши (репорт 2026-08-09): клик по журналу переводит фокус на
/// прокрутку (↑↓ листают журнал, а не историю команд), клик по полю
/// ввода или любая печать — обратно.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Focus {
    Input,
    Log,
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
    /// Явно выбранный пользователем провайдер (/model) — пин на сессию:
    /// без него пул по требованию Any мог выбрать ДРУГОГО провайдера,
    /// делая выбор пользователя декорацией (репорт 2026-08-03: выбран
    /// deepseek, ход ушёл в kimi → 401).
    active_provider: Option<String>,
    /// Открытый запрос подтверждения (модал y/n).
    confirm_prompt: Option<String>,
    /// Выбранный вариант модала: 0=да, 1=для сессии, 2=для проекта,
    /// 3=нет (умолчание — «нет», безопасно).
    confirm_selection: usize,
    /// Выбранный вариант в модале (подсветка): false = «Нет»
    /// (безопасное умолчание), true = «Да». Enter активирует ВЫБРАННОЕ
    /// — репорт 2026-08-03: Enter раньше означал «нет», что против
    /// интуиции пользователя («я ответил Да, но не прошло»).
    /// Разрешения «для сессии»: имена инструментов, разрешённых на
    /// время этого запуска чата (общие с воркером).
    session_grants: std::sync::Arc<std::sync::Mutex<std::collections::HashSet<String>>>,
    /// Установленные скилы (§20.16): триггер — кодом, потолок — фильтром.
    skills: Vec<crate::skills::Skill>,
    /// Установленные субагенты (§20.36: видны и устанавливаются из TUI,
    /// не только CLI).
    agents: Vec<crate::agents::AgentDef>,
    /// URL репозитория плагина, набранный в `Flow::PluginAskRepo` и
    /// ГОТОВЫЙ к запуску (§20.36): установка плагина — полноценный
    /// process-engine-инстанс с человеческим гейтом (TOFU доверия
    /// репозиторию) — не переписывается под канал TUI, вместо этого
    /// `event_loop` временно ПРИОСТАНАВЛИВАЕТ альтернативный экран/raw
    /// mode и прогоняет реальный `plugin_install::run` как в CLI, потом
    /// восстанавливает TUI. Поле — сигнал из `handle_key` (без доступа
    /// к `Terminal`) в `event_loop` (где `Terminal` есть).
    pending_plugin_install: Option<String>,
    /// Ответ воркеру на подтверждение (канал создаётся на ход).
    answer_tx: Option<Sender<ConfirmAnswer>>,
    /// human.ask (B7): активный вопрос агента (модал с вводом), буфер
    /// ответа и канал ответа воркеру (создаётся на ход, как answer_tx;
    /// НЕ take при ответе — вопросов за ход может быть несколько).
    ask_prompt: Option<String>,
    ask_input: String,
    ask_answer_tx: Option<Sender<Result<String, String>>>,
    /// Фокус мыши: журнал (прокрутка) или поле ввода (по умолчанию).
    focus: Focus,
    /// Мышь захвачена (колесо/клик) или отпущена (`/mouse` — нативное
    /// выделение текста; репорт 2026-08-09: «перестало работать
    /// копирование»). Захвачена по умолчанию — как у Claude Code.
    mouse_capture: bool,
    /// Локаль интерфейса (i18n, 2026-08-09): `[ui] locale` из конфига,
    /// иначе окружение, иначе ru. Смена — `/config locale`.
    locale: Locale,
    /// Области журнала и поля ввода последнего кадра — для hit-test
    /// кликов и колеса (записываются в `draw`, читаются в
    /// `handle_mouse`).
    log_area: Rect,
    input_area: Rect,
    /// Полоса прокрутки журнала (0.35.1): область и последний известный
    /// max_scroll — для мышиного взаимодействия (клик по дорожке =
    /// переход к позиции, драг = протяжка). Обновляются каждым кадром.
    log_bar_area: Option<Rect>,
    log_max_scroll: usize,
    /// Прокрутка многострочного поля ввода (первая видимая экранная
    /// строка): поле растёт до потолка, дальше крутится само.
    input_scroll: u16,
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
    let response = request
        .send()
        .and_then(|r| r.error_for_status())
        .map_err(|e| {
            let status = e.status().map(|s| s.as_u16());
            match status {
                Some(401) | Some(403) => {
                    format!("{url}: HTTP {} — ключ не принят", status.unwrap())
                }
                Some(code) => format!("{url}: HTTP {code}"),
                None => format!("{url}: {e}"),
            }
        })?;
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
    let locale = Locale::resolve(config.ui.locale.as_deref());
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
        active_provider: None,
        confirm_prompt: None,
        confirm_selection: 4,
        session_grants: std::sync::Arc::new(
            std::sync::Mutex::new(std::collections::HashSet::new()),
        ),
        skills: crate::skills::load_all(&std::env::current_dir().unwrap_or_default()),
        agents: crate::agents::load_all(&std::env::current_dir().unwrap_or_default()),
        pending_plugin_install: None,
        answer_tx: None,
        ask_prompt: None,
        ask_input: String::new(),
        ask_answer_tx: None,
        focus: Focus::Input,
        mouse_capture: true,
        locale,
        log_area: Rect::default(),
        log_bar_area: None,
        log_max_scroll: 0,
        input_area: Rect::default(),
        input_scroll: 0,
        tx,
        rx,
        done: false,
    };
    // Подхват ленты прошлых сессий этой области (§20.15).
    let workspace = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let resumed = crate::chat_history::load(&workspace);
    if !resumed.is_empty() {
        app.sys(format!(
            "подхвачено сообщений прошлых сессий: {}",
            resumed.len()
        ));
    }
    app.conversation = resumed;
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
///
/// Мышь захватывается (репорт 2026-08-09: «колесо листает историю
/// команд в поле ввода» — без захвата терминал в alternate screen шлёт
/// колесо как стрелки ↑↓). Колесо крутит область под указателем
/// (журнал или многострочное поле ввода), клик переводит фокус —
/// см. `handle_mouse`. Цена известна: нативное выделение текста
/// требует Shift+drag — это отражено в строке подсказок.
struct TerminalGuard(Terminal<ratatui::backend::CrosstermBackend<Stdout>>);

impl TerminalGuard {
    fn new() -> std::io::Result<Self> {
        enable_raw_mode()?;
        let mut stdout = std::io::stdout();
        stdout.execute(EnterAlternateScreen)?;
        // Репорт 2026-08-09: без bracketed paste терминал в raw-режиме
        // шлёт вставленный многострочный текст как ОТДЕЛЬНЫЕ нажатия
        // Enter на каждый перевод строки — вставка спеки с заголовками
        // разбивалась на десяток отдельных сообщений, обрывающихся
        // ровно на заголовках. С этим режимом вся вставка приходит
        // ОДНИМ `Event::Paste` — см. обработку в `event_loop`.
        stdout.execute(EnableBracketedPaste)?;
        // Захват — ПОСЛЕ EnterAlternateScreen (тот же день: колесо без
        // него улетает в историю команд); снятие — в Drop, чтобы паника
        // не оставила терминал с захваченной мышью.
        stdout.execute(EnableMouseCapture)?;
        let backend = ratatui::backend::CrosstermBackend::new(stdout);
        Ok(Self(Terminal::new(backend)?))
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = std::io::stdout().execute(DisableMouseCapture);
        let _ = std::io::stdout().execute(DisableBracketedPaste);
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
                WorkerMsg::Sys(text) => app.sys(text),
                WorkerMsg::ConfirmRequest(prompt) => {
                    app.confirm_prompt = Some(prompt);
                }
                WorkerMsg::AskRequest(question) => {
                    app.ask_prompt = Some(question);
                    app.ask_input.clear();
                }
                WorkerMsg::Reply(Ok(reply)) => {
                    app.busy = false;
                    // Лента пишется парой user+assistant на ответ
                    // (§20.15); user-запись уже в conversation.
                    if let Some(user) = app
                        .conversation
                        .last()
                        .and_then(|m| m.get("content"))
                        .and_then(Value::as_str)
                        .map(str::to_string)
                    {
                        let workspace =
                            std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
                        crate::chat_history::append(&workspace, &user, &reply);
                    }
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
            match event::read().map_err(|e| RunError::BadInput(e.to_string()))? {
                Event::Key(key) => handle_key(app, key),
                Event::Mouse(mouse) => handle_mouse(app, mouse),
                Event::Paste(text) => handle_paste(app, &text),
                _ => {}
            }
        } else if app.busy {
            app.spinner_frame += 1; // тик спиннера по таймауту poll
        }

        // /plugins add (§20.36): установка плагина — реальный
        // process-engine-инстанс с human_gate (TOFU доверия репозиторию),
        // подключённый к блокирующему stdin — как в CLI, не переписан
        // под канал TUI. Вместо того чтобы дублировать security-
        // критичный код, TUI временно ВЫХОДИТ из alternate screen/raw
        // mode (тот же приём, что у редактора коммит-сообщения git),
        // прогоняет тот же `plugin_install::run`, что CLI, и
        // восстанавливается. `handle_key` не может сделать это сама —
        // ей не передан `Terminal`; сигнал — `pending_plugin_install`.
        if let Some(repo) = app.pending_plugin_install.take() {
            suspend_and_install_plugin(app, terminal, &repo);
        }
    }
    Ok(())
}

/// См. комментарий в `event_loop` у места вызова.
fn suspend_and_install_plugin(
    app: &mut App,
    terminal: &mut Terminal<ratatui::backend::CrosstermBackend<Stdout>>,
    repo: &str,
) {
    let _ = terminal.backend_mut().execute(LeaveAlternateScreen);
    // Вне TUI мышь и bracketed paste не нужны (выделение текста в
    // выводе установщика обязано работать нативно); на возврате —
    // включить обратно, иначе колесо снова уйдёт в историю команд.
    let _ = terminal.backend_mut().execute(DisableMouseCapture);
    let _ = terminal.backend_mut().execute(DisableBracketedPaste);
    let _ = disable_raw_mode();
    println!("[berimor] установка плагина из {repo} (как в CLI berimor plugin install):");
    let result = crate::plugin_install::run(&app.config, repo, &None, None, None, None);
    let _ = enable_raw_mode();
    let _ = terminal.backend_mut().execute(EnterAlternateScreen);
    let _ = terminal.backend_mut().execute(EnableBracketedPaste);
    let _ = terminal.backend_mut().execute(EnableMouseCapture);
    let _ = terminal.clear(); // экран вне TUI мог оставить чужой контент
    match result {
        Ok(()) => app.sys(format!("плагин установлен из {repo}")),
        Err(err) => app.sys(format!("установка плагина не удалась: {err}")),
    }
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

    fn slash_filtered(&self) -> Vec<&'static (&'static str, SlashAbout)> {
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
        // Репорт 2026-08-09: без этой защиты быстрый повторный Enter
        // (в частности — вставка многострочного текста БЕЗ bracketed
        // paste, где каждый \n синтетически становится Enter) запускал
        // ВТОРОЙ воркер, пока первый ещё не ответил. Оба треда шлют
        // `WorkerMsg::Reply`, и обработчик берёт `conversation.last()`
        // по ВРЕМЕНИ ПРИХОДА ответа, не по своему ходу — второй ответ
        // приходит уже ПОСЛЕ push первого ассистентского сообщения, и
        // `chat_history::append` записывает ответ модели как будто это
        // ввод пользователя (зеркалирование ленты, зацикливание агента
        // на переспрашивании самого себя).
        if self.busy {
            return;
        }
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
        // Триггер скилла — КОДОМ (§20.16): префикс фразы; потолок —
        // фильтр диспетча на этот ход.
        let matched = crate::skills::match_trigger(&self.skills, &message).map(|s| {
            (
                s.name.clone(),
                s.version.clone(),
                s.body.clone(),
                s.tools.clone(),
            )
        });
        match matched {
            Some((name, version, body, tools)) => {
                self.sys(format!("скилл «{name}» активен (триггер фразы)"));
                let ceiling = if tools.is_empty() { None } else { Some(tools) };
                let augmented = format!(
                    "[Активен скилл «{name}» v{version}. Следуй его инструкциям:\n{body}]\n\nЗапрос пользователя: {message}"
                );
                self.start_turn(augmented, ceiling);
            }
            None => self.start_turn(message, None),
        }
    }

    /// Ход агента — в воркер-потоке с собственным рантаймом (UI не
    /// блокируется; конфиг клонируется — «перезагрузка» бесплатна).
    fn start_turn(&mut self, message: String, tool_ceiling: Option<Vec<String>>) {
        self.busy = true;
        let mut config = self.config.clone();
        // Пин провайдера (/model): явный выбор пользователя сильнее
        // автоотбора пула — воркер видит только выбранного.
        if let Some(name) = &self.active_provider {
            config.providers.retain(|p| &p.name == name);
        }
        let conversation = self.conversation.clone();
        let tx = self.tx.clone();
        // Канал ответов на подтверждения: воркер спрашивает (модал в
        // TUI), UI отвечает; живёт до конца хода.
        let (answer_tx, answer_rx) = channel::<ConfirmAnswer>();
        self.answer_tx = Some(answer_tx);
        // Канал ответов human.ask (B7): воркер шлёт вопрос (модал с
        // вводом в TUI), UI возвращает строку/отмену; живёт до конца хода.
        let (ask_tx, ask_rx) = channel::<Result<String, String>>();
        self.ask_answer_tx = Some(ask_tx);
        let session_grants = self.session_grants.clone();
        std::thread::spawn(move || {
            let reply = crate::chat::execute_turn(
                &config,
                conversation,
                message,
                tx.clone(),
                crate::chat::TurnChannels {
                    answer_rx: Some(answer_rx),
                    ask_rx: Some(ask_rx),
                },
                session_grants,
                tool_ceiling,
            );
            let _ = tx.send(WorkerMsg::Reply(reply));
        });
    }

    fn run_command(&mut self, command: &str) {
        match command {
            "exit" | "quit" => self.done = true,
            // Репорт 2026-08-09: «перестали работать выделение и
            // копирование» — неизбежная цена захвата мыши (у Claude
            // Code — та же модель: полный захват, выделение через
            // Shift). Переключатель отпускает захват: выделение снова
            // нативное, колесо и клик-фокус отключаются до повторного
            // /mouse.
            "mouse" => {
                self.mouse_capture = !self.mouse_capture;
                let mut out = std::io::stdout();
                let strings = i18n::strings(self.locale);
                if self.mouse_capture {
                    let _ = out.execute(EnableMouseCapture);
                    self.sys(strings.sys_mouse_on);
                } else {
                    let _ = out.execute(DisableMouseCapture);
                    self.sys(strings.sys_mouse_off);
                }
            }
            // Копирование без выделения: последний ответ агента — в
            // буфер обмена внешней утилитой (без новых зависимостей).
            "copy" => {
                let last = self.log.iter().rev().find_map(|line| match line {
                    LogLine::Assistant(text) => Some(text.clone()),
                    _ => None,
                });
                let strings = i18n::strings(self.locale);
                match last {
                    Some(text) => match copy_to_clipboard(&text) {
                        Some(tool) => self.sys(format!("{} ({tool})", strings.sys_copied)),
                        None => self.sys(strings.sys_no_clipboard),
                    },
                    None => self.sys(strings.sys_nothing_to_copy),
                }
            }
            "help" => {
                let strings = i18n::strings(self.locale);
                for (name, about) in SLASH_COMMANDS {
                    self.sys(format!("{name:<14} — {}", about(strings)));
                }
            }
            // /config — МЕНЮ параметров (директива 2026-08-12: «/config
            // > модалка > Locale > модалка языка»); показ конфигурации —
            // один из пунктов. /config locale [код] — шорткат мимо меню.
            cmd if cmd == "config" || cmd.starts_with("config locale") => {
                let strings = i18n::strings(self.locale);
                if cmd == "config" {
                    let items: Vec<String> = vec![
                        strings.settings_show.to_string(),
                        format!(
                            "{} — {} ({})",
                            strings.settings_locale,
                            self.locale.native_name(),
                            self.locale.code()
                        ),
                    ];
                    self.picker = Some(Picker::new(strings.settings_menu_title, items, false));
                    self.flow = Some(Flow::ConfigMenu);
                    return;
                }
                let arg = cmd.strip_prefix("config locale").unwrap().trim();
                if arg.is_empty() {
                    self.open_locale_picker();
                } else {
                    match Locale::from_code(arg) {
                        Some(locale) => self.apply_locale(locale),
                        None => self.sys(format!("{arg} — {}", strings.sys_locale_unknown)),
                    }
                }
            }
            "models" => {
                if self.config.providers.is_empty() {
                    self.sys(i18n::strings(self.locale).sys_providers_empty);
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
                    i18n::strings(self.locale).picker_presets,
                    items,
                    true,
                ));
                self.flow = Some(Flow::AddPickPresets);
            }
            "model" => {
                if self.config.providers.is_empty() {
                    self.sys(i18n::strings(self.locale).sys_providers_empty);
                    return;
                }
                let items: Vec<String> = self
                    .config
                    .providers
                    .iter()
                    .map(|p| format!("{} — {}", p.name, p.model_id))
                    .collect();
                self.picker = Some(Picker::new(
                    i18n::strings(self.locale).picker_provider,
                    items,
                    false,
                ));
                self.flow = Some(Flow::SwitchPickProvider);
            }
            "tools" => {
                let catalog = crate::chat::tools_catalog(&self.config);
                let lines: Vec<String> = catalog
                    .as_array()
                    .map(|items| {
                        items
                            .iter()
                            .map(|tool| {
                                let name = tool.get("name").and_then(Value::as_str).unwrap_or("?");
                                let about = tool.get("about").and_then(Value::as_str).unwrap_or("");
                                format!("{name} — {about}")
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                for line in lines {
                    self.sys(line);
                }
            }
            "skills" => {
                if self.skills.is_empty() {
                    self.sys("скиллы не установлены — berimor skill list --available");
                }
                let lines: Vec<String> = self
                    .skills
                    .iter()
                    .map(|skill| {
                        format!(
                            "{} v{} — {} [{}]{} · {}",
                            skill.name,
                            skill.version,
                            skill.description,
                            skill.triggers.join(", "),
                            if skill.tools.is_empty() {
                                String::new()
                            } else {
                                format!(" · потолок: {}", skill.tools.join(" "))
                            },
                            skill.origin.display()
                        )
                    })
                    .collect();
                for line in lines {
                    self.sys(line);
                }
            }
            "skills add" => self.open_ext_catalog_picker(crate::ext_cmd::ExtKind::Skill),
            "skills remove" => self.open_ext_remove_picker(crate::ext_cmd::ExtKind::Skill),
            "agents" => {
                if self.agents.is_empty() {
                    self.sys("субагенты не установлены — /agents add");
                }
                let lines: Vec<String> = self
                    .agents
                    .iter()
                    .map(|agent| format!("{} — {}", agent.name, agent.description))
                    .collect();
                for line in lines {
                    self.sys(line);
                }
            }
            "agents add" => self.open_ext_catalog_picker(crate::ext_cmd::ExtKind::Agent),
            "agents remove" => self.open_ext_remove_picker(crate::ext_cmd::ExtKind::Agent),
            "plugins" => {
                let dispatch = crate::plugin_runtime::PluginRuntimeDispatch::scan(
                    &crate::plugin_install::plugins_root_dir(),
                );
                if dispatch.is_empty() {
                    self.sys("плагины не установлены — /plugins add <repo>");
                }
                for (name, tools) in dispatch.summaries() {
                    let tools = if tools.is_empty() {
                        "без инструментов".to_string()
                    } else {
                        tools.join(", ")
                    };
                    self.sys(format!("{name} — {tools}"));
                }
            }
            "plugins add" => {
                self.flow = Some(Flow::PluginAskRepo {
                    input: String::new(),
                });
            }
            "plugins remove" => {
                let dispatch = crate::plugin_runtime::PluginRuntimeDispatch::scan(
                    &crate::plugin_install::plugins_root_dir(),
                );
                let names: Vec<String> = dispatch.summaries().into_iter().map(|(n, _)| n).collect();
                if names.is_empty() {
                    self.sys("ничего не установлено");
                    return;
                }
                let items = names.clone();
                self.picker = Some(Picker::new(
                    i18n::strings(self.locale).picker_remove,
                    items,
                    false,
                ));
                self.flow = Some(Flow::PluginRemovePick { names });
            }
            other => {
                // Slash-триггер скилла (§20.16): неизвестная встроенная
                // команда может быть триггером — тогда это сообщение агенту.
                let original = format!("/{other}");
                let matched = crate::skills::match_trigger(&self.skills, &original).map(|s| {
                    (
                        s.name.clone(),
                        s.version.clone(),
                        s.body.clone(),
                        s.tools.clone(),
                    )
                });
                match matched {
                    Some((name, version, body, tools)) => {
                        self.sys(format!(
                            "скилл «{name}» активен (триггер /{})",
                            other.split_whitespace().next().unwrap_or(other)
                        ));
                        self.log.push(LogLine::User(original.clone()));
                        self.conversation
                            .push(serde_json::json!({"role": "user", "content": original.clone()}));
                        self.maybe_follow();
                        let ceiling = if tools.is_empty() { None } else { Some(tools) };
                        let augmented = format!(
                            "[Активен скилл «{name}» v{version}. Следуй его инструкциям:\n{body}]\n\nЗапрос пользователя: {original}"
                        );
                        self.start_turn(augmented, ceiling);
                    }
                    None => self.sys(format!("неизвестная команда /{other} — /help")),
                }
            }
        }
    }

    /// Продвижение многошагового сценария после подтверждения пикера.
    /// Применить локаль интерфейса (i18n, 2026-08-09): на сессию
    /// сразу + персистентно в локальный конфиг (`[ui] locale`).
    /// Сбой записи не отменяет смену на сессию — честное сообщение.
    fn apply_locale(&mut self, locale: Locale) {
        self.locale = locale;
        let strings = i18n::strings(locale);
        match crate::setup::set_locale_in_local_config(locale.code()) {
            Ok(path) => self.sys(format!(
                "{}{} ({}) {} — {path}",
                strings.sys_config_locale,
                locale.native_name(),
                locale.code(),
                strings.sys_locale_set
            )),
            Err(err) => self.sys(format!(
                "{}{} ({}) — на сессию; не сохранено: {err}",
                strings.sys_config_locale,
                locale.native_name(),
                locale.code()
            )),
        }
    }

    /// Пикер локалей: 8 самоназваний с кодом, текущая предвыбрана
    /// (используется из /config locale и из меню /config, 2026-08-12).
    fn open_locale_picker(&mut self) {
        let strings = i18n::strings(self.locale);
        let items: Vec<String> = Locale::ALL
            .iter()
            .map(|l| format!("{} ({})", l.native_name(), l.code()))
            .collect();
        let current = Locale::ALL.iter().position(|l| *l == self.locale);
        let mut picker = Picker::new(strings.picker_locale, items, false);
        picker.state.select(current.or(Some(0)));
        self.picker = Some(picker);
        self.flow = Some(Flow::LocalePick);
    }

    /// Печать эффективной конфигурации в журнал (пункт меню /config).
    fn print_config_info(&mut self) {
        let strings = i18n::strings(self.locale);
        self.sys(format!(
            "{}{}",
            strings.sys_config_journal,
            self.config.storage_path.display()
        ));
        self.sys(format!(
            "{}{:?}",
            strings.sys_config_mode, self.config.confirmation_mode
        ));
        self.sys(format!(
            "{}{}",
            strings.sys_config_providers,
            self.config.providers.len()
        ));
        self.sys(format!(
            "{}{} ({})",
            strings.sys_config_locale,
            self.locale.native_name(),
            self.locale.code()
        ));
    }

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
                if model.starts_with("sk-") {
                    // Ключ на месте модели — защита от повтора инцидента
                    // (старый мастер принял ключ в поле model_id).
                    self.sys("«sk-…» похоже на ключ, не на модель — отклонено");
                    self.next_preset();
                    return;
                }
                let provider = presets::instantiate(preset, Some(model), None);
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
                self.active_provider = Some(provider_name.clone());
                // «Закрепить навсегда» (репорт 2026-08-03): model_id — в
                // локальный конфиг (слой сильнее глобального; путь —
                // config::default_config_path(), §20.X 2026-08-09).
                let pin_note = match self
                    .config
                    .providers
                    .iter()
                    .find(|p| p.name == provider_name)
                    .map(crate::setup::pin_model_to_local_config)
                {
                    Some(Ok(path)) => format!("; закреплено навсегда в {path}"),
                    Some(Err(err)) => format!("; НЕ закреплено: {err}"),
                    None => String::new(),
                };
                self.sys(format!(
                    "модель сессии: {provider_name} → {model}{pin_note}"
                ));
            }
            Flow::AddAskKey { .. } => {} // обрабатывается в вводе, не пикером
            Flow::PluginAskRepo { .. } => {} // обрабатывается в вводе, не пикером
            Flow::LocalePick => {
                let Some(locale) = picker.selected().and_then(|i| Locale::ALL.get(i)) else {
                    return;
                };
                self.apply_locale(*locale);
            }
            // Меню параметров /config (2026-08-12): 0 — показать
            // конфигурацию, 1 — пикер локали (модалка языка).
            Flow::ConfigMenu => match picker.selected() {
                Some(0) => self.print_config_info(),
                Some(1) => self.open_locale_picker(),
                _ => {}
            },
            Flow::ExtCatalogPick { kind, names } => {
                let Some(name) = picker.selected().and_then(|i| names.get(i)).cloned() else {
                    return;
                };
                let root = match crate::ext_cmd::dest_root(&kind, false) {
                    Ok(root) => root,
                    Err(err) => {
                        self.sys(err);
                        return;
                    }
                };
                match crate::catalog::install(kind.default_repo(), kind.prefix(), &name, &root) {
                    Ok(path) => {
                        self.sys(format!("установлено: {}", path.display()));
                        self.reload_extensions();
                    }
                    Err(err) => self.sys(format!("установка не удалась: {err}")),
                }
            }
            Flow::ExtRemovePick { kind, names } => {
                let Some(name) = picker.selected().and_then(|i| names.get(i)).cloned() else {
                    return;
                };
                match crate::ext_cmd::remove_installed(&kind, &name, false) {
                    Ok(path) => {
                        self.sys(format!("удалено: {}", path.display()));
                        self.reload_extensions();
                    }
                    Err(err) => self.sys(err),
                }
            }
            Flow::PluginRemovePick { names } => {
                let Some(name) = picker.selected().and_then(|i| names.get(i)).cloned() else {
                    return;
                };
                match crate::plugin_install::remove(&name) {
                    Ok(path) => self.sys(format!("удалено: {}", path.display())),
                    Err(err) => self.sys(err),
                }
            }
        }
    }

    /// Следующий пресет из очереди /models add. Порядок для облачных
    /// провайдеров: КЛЮЧ → потом список моделей — `/models` у DeepSeek
    /// и др. требует авторизации (401 без ключа; репорт 2026-08-03
    /// «провайдер не доступен»). Ключ вводится первым и сразу
    /// действует в этой сессии (set_var), не только в secrets.env.
    fn next_preset(&mut self) {
        let Some(preset) = self.pending_presets.first().copied() else {
            self.finish_add();
            return;
        };
        self.pending_presets.remove(0);
        if let Some(env_name) = preset.key_env {
            if std::env::var_os(env_name).is_none() {
                self.flow = Some(Flow::AddAskKey {
                    preset,
                    key_env: env_name.to_string(),
                    input: String::new(),
                });
                return;
            }
        }
        self.pick_model_for(preset);
    }

    /// Пикер модели пресета из живого списка провайдера.
    fn pick_model_for(&mut self, preset: &'static ProviderPreset) {
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

    /// /skills add, /agents add (§20.36): живой git-каталог — тот же
    /// приём, что `fetch_models` для провайдеров.
    fn open_ext_catalog_picker(&mut self, kind: crate::ext_cmd::ExtKind) {
        let repo = kind.default_repo();
        match crate::catalog::sync(repo) {
            Ok(dir) => {
                let entries = crate::catalog::list(&dir, kind.prefix(), kind.marker());
                if entries.is_empty() {
                    self.sys(format!("каталог пуст ({})", kind.prefix()));
                    return;
                }
                let names: Vec<String> = entries.iter().map(|e| e.name.clone()).collect();
                let items: Vec<String> = entries
                    .iter()
                    .map(|e| format!("{} — {}", e.name, e.summary))
                    .collect();
                self.picker = Some(Picker::new(
                    format!("Каталог {} (Enter — установить)", kind.prefix()),
                    items,
                    false,
                ));
                self.flow = Some(Flow::ExtCatalogPick { kind, names });
            }
            Err(err) => self.sys(format!("каталог недоступен: {err}")),
        }
    }

    /// /skills remove, /agents remove: пикер из уже установленных.
    fn open_ext_remove_picker(&mut self, kind: crate::ext_cmd::ExtKind) {
        let names: Vec<String> = match kind {
            crate::ext_cmd::ExtKind::Skill => self.skills.iter().map(|s| s.name.clone()).collect(),
            crate::ext_cmd::ExtKind::Agent => self.agents.iter().map(|a| a.name.clone()).collect(),
        };
        if names.is_empty() {
            self.sys("ничего не установлено");
            return;
        }
        let items = names.clone();
        self.picker = Some(Picker::new(
            i18n::strings(self.locale).picker_remove,
            items,
            false,
        ));
        self.flow = Some(Flow::ExtRemovePick { kind, names });
    }

    /// Перечитывает установленные скилы/субагенты после install/remove
    /// (§20.36) — без этого требовался бы перезапуск `berimor` для
    /// подхвата изменения, сделанного в ЭТОЙ же сессии.
    fn reload_extensions(&mut self) {
        let workspace = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        self.skills = crate::skills::load_all(&workspace);
        self.agents = crate::agents::load_all(&workspace);
    }
}

fn handle_key(app: &mut App, key: KeyEvent) {
    // Терминалы с расширенным клавиатурным протоколом (kitty и др.)
    // шлют события И на отпускание клавиши: без фильтра каждое
    // нажатие обрабатывается ДВАЖДЫ — стрелка в модале подтверждения
    // переключала выбор на «да» по Press и обратно на «нет» по
    // Release, Enter получал «нет» (репорт 2026-08-03: «двигал
    // стрелку, потом Enter — всё равно отказ»; сюда же «буквы через
    // раз»). На обычных терминалах kind всегда Press — фильтр
    // ничего не меняет.
    if key.kind == crossterm::event::KeyEventKind::Release {
        return;
    }
    if key.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('d'))
    {
        app.done = true;
        return;
    }

    // Модал подтверждения действия (capability-гейт) — выше всего.
    // Четыре варианта с областью действия (0.14.0): да / для сессии /
    // для проекта / нет. ←→/Tab двигают подсветку, Enter активирует
    // ВЫБРАННОЕ (умолчание-подсветка — «Нет»); буквы — сразу.
    if app.confirm_prompt.is_some() {
        match key.code {
            // Обе оси (репорт 0.14.0: пикеры ходят ↑↓, модал учил ←→ —
            // пользователь жмёт «вниз», модал глух, Enter на «нет» —
            // отказ при намерении «проект»).
            KeyCode::Left | KeyCode::Up | KeyCode::BackTab => {
                app.confirm_selection = (app.confirm_selection + 4) % 5;
            }
            KeyCode::Right | KeyCode::Down | KeyCode::Tab => {
                app.confirm_selection = (app.confirm_selection + 1) % 5;
            }
            _ => {
                let answer = match key.code {
                    KeyCode::Char('y')
                    | KeyCode::Char('Y')
                    | KeyCode::Char('д')
                    | KeyCode::Char('Д') => Some(ConfirmAnswer::Once),
                    KeyCode::Char('с')
                    | KeyCode::Char('С')
                    | KeyCode::Char('s')
                    | KeyCode::Char('S') => Some(ConfirmAnswer::Session),
                    KeyCode::Char('п')
                    | KeyCode::Char('П')
                    | KeyCode::Char('p')
                    | KeyCode::Char('P') => Some(ConfirmAnswer::Project),
                    KeyCode::Char('n')
                    | KeyCode::Char('N')
                    | KeyCode::Char('н')
                    | KeyCode::Char('Н')
                    | KeyCode::Esc => Some(ConfirmAnswer::Deny),
                    KeyCode::Char('в')
                    | KeyCode::Char('В')
                    | KeyCode::Char('a')
                    | KeyCode::Char('A') => Some(ConfirmAnswer::ProjectAll),
                    KeyCode::Enter => Some(match app.confirm_selection {
                        0 => ConfirmAnswer::Once,
                        1 => ConfirmAnswer::Session,
                        2 => ConfirmAnswer::Project,
                        3 => ConfirmAnswer::ProjectAll,
                        _ => ConfirmAnswer::Deny,
                    }),
                    _ => None,
                };
                if let Some(answer) = answer {
                    if let Some(tx) = app.answer_tx.take() {
                        let _ = tx.send(answer);
                    }
                    app.confirm_prompt = None;
                    app.confirm_selection = 4;
                }
            }
        }
        return;
    }

    // Ввод ключа API (маскируемый) — отдельный режим.
    if let Some(Flow::AddAskKey { input, .. }) = &mut app.flow {
        match key.code {
            KeyCode::Enter => {
                if let Some(Flow::AddAskKey {
                    preset,
                    key_env,
                    input,
                }) = app.flow.take()
                {
                    if !input.is_empty() {
                        // В сессию — сразу (список моделей и ходы агента
                        // используют ключ уже сейчас), в secrets.env —
                        // при finish_add.
                        std::env::set_var(&key_env, &input);
                        app.staging_keys.push((key_env, input));
                    }
                    app.pick_model_for(preset);
                }
            }
            KeyCode::Esc => {
                if let Some(Flow::AddAskKey { preset, .. }) = app.flow.take() {
                    app.pick_model_for(preset);
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

    // Ввод URL репозитория плагина (не маскируется — не секрет).
    // Реальный запуск установки — не здесь: только копим `input` и
    // сигналим через `pending_plugin_install`, потому что установка
    // плагина требует ПРИОСТАНОВКИ TUI (нужен `Terminal`, которого
    // здесь нет) — обрабатывает `event_loop` после этого вызова.
    if let Some(Flow::PluginAskRepo { input }) = &mut app.flow {
        match key.code {
            KeyCode::Enter => {
                if let Some(Flow::PluginAskRepo { input }) = app.flow.take() {
                    let repo = input.trim().to_string();
                    if repo.is_empty() {
                        app.sys("пустой URL — отменено");
                    } else {
                        app.pending_plugin_install = Some(repo);
                    }
                }
            }
            KeyCode::Esc => app.flow = None,
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

    // Модал human.ask (B7): свободный ответ текстом. Enter — отправить
    // ответ воркеру, Esc — отмена (инструмент получает ошибку, цикл
    // агента не висит). Канал НЕ забирается (take) — вопросов за ход
    // может быть несколько.
    if app.ask_prompt.is_some() {
        match key.code {
            KeyCode::Enter => {
                let answer = std::mem::take(&mut app.ask_input);
                app.ask_prompt = None;
                if let Some(tx) = &app.ask_answer_tx {
                    let _ = tx.send(Ok(answer));
                }
            }
            KeyCode::Esc => {
                app.ask_prompt = None;
                app.ask_input.clear();
                if let Some(tx) = &app.ask_answer_tx {
                    let _ = tx.send(Err("отменено пользователем".to_string()));
                }
            }
            KeyCode::Char(c) => app.ask_input.push(c),
            KeyCode::Backspace => {
                app.ask_input.pop();
            }
            _ => {}
        }
        return;
    }

    // Фокус на журнале (клик мышью, репорт 2026-08-09): стрелки ↑↓
    // листают ЖУРНАЛ, а не историю команд поля ввода; Esc — назад в
    // ввод; любая печать/Enter возвращает фокус и обрабатывается уже
    // как ввод (символ не теряется).
    if app.focus == Focus::Log {
        match key.code {
            KeyCode::Esc => {
                app.focus = Focus::Input;
                return;
            }
            KeyCode::Up => {
                app.follow_tail = false;
                app.scroll = app.scroll.saturating_sub(1).min(u16::MAX - 1);
                if app.scroll == u16::MAX {
                    app.scroll = 0;
                }
                return;
            }
            KeyCode::Down => {
                app.scroll = app.scroll.saturating_add(1);
                app.follow_tail = false;
                return;
            }
            // PgUp/PgDn — общий скролл ниже, фокус журнала сохраняется.
            KeyCode::PageUp | KeyCode::PageDown => {}
            _ => app.focus = Focus::Input,
        }
    }

    match key.code {
        // Alt+Enter — ручной перевод строки (многострочный ввод,
        // репорт 2026-08-09: «сделал бы многострочное поле ввода»).
        // Обычный Enter остаётся отправкой. Shift+Enter большинству
        // терминалов без расширенного клавиатурного протокола
        // неотличим от Enter — не используем.
        KeyCode::Enter if key.modifiers.contains(KeyModifiers::ALT) => {
            app.input.insert(app.cursor, '\n');
            app.cursor += 1;
        }
        // Enter при открытой палитре — выбранная команда, а не набранный
        // фрагмент (репорт 2026-08-03: «выбор в палитре не учитывается»).
        // Tab — только подстановка, без отправки.
        KeyCode::Enter if app.slash_open => {
            app.apply_slash_completion();
            app.submit();
        }
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
            // Многострочный буфер: ↑↓ двигают курсор по строкам
            // (репорт 2026-08-09); история команд — только когда поле
            // однострочное, иначе ↑ на первой строке молча затирал бы
            // набранный многострочный черновик.
            if app.input.contains('\n') {
                move_cursor_vertical(app, -1);
            } else if !app.history.is_empty() {
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
            if app.input.contains('\n') {
                move_cursor_vertical(app, 1);
            } else if let Some(idx) = app.history_idx {
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

/// Bracketed paste (репорт 2026-08-09): весь вставленный текст — ОДНИМ
/// событием, не синтетическими нажатиями Enter на каждый перевод строки
/// (как было бы без `EnableBracketedPaste` — см. `TerminalGuard::new`).
/// Поле сообщения — многострочное (тот же репорт): переводы строк
/// сохраняются (нормализация \r\n/\r → \n), вставка остаётся ОДНИМ
/// сообщением на выходе.
fn handle_paste(app: &mut App, text: &str) {
    // Активный текстовый ввод (ключ API / URL плагина) — однострочный:
    // пробельные схлопываются, переводы строк в ключ не попадают.
    if let Some(Flow::AddAskKey { input, .. } | Flow::PluginAskRepo { input }) = &mut app.flow {
        let normalized: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
        input.push_str(&normalized);
        return;
    }
    // Модал/пикер — вставлять некуда, молча игнорируем.
    if app.confirm_prompt.is_some() || app.picker.is_some() {
        return;
    }
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    if normalized.trim().is_empty() {
        return;
    }
    app.input.insert_str(app.cursor, &normalized);
    app.cursor += normalized.len();
}

/// Мышь (репорт 2026-08-09): колесо крутит область ПОД УКАЗАТЕЛЕМ —
/// журнал (шаг 3, семантика PageUp/Down: u16::MAX — «прижат к низу»)
/// или многострочное поле ввода. Клик левой кнопкой переводит фокус:
/// по журналу — прокрутка стрелками, по полю ввода — обратно.
fn handle_mouse(app: &mut App, mouse: MouseEvent) {
    use crossterm::event::{MouseButton, MouseEventKind};
    let in_log = point_in_rect(app.log_area, mouse.column, mouse.row);
    let in_input = point_in_rect(app.input_area, mouse.column, mouse.row);
    match mouse.kind {
        MouseEventKind::ScrollUp => {
            if in_input {
                app.input_scroll = app.input_scroll.saturating_sub(3);
            } else {
                app.follow_tail = false;
                app.scroll = app.scroll.saturating_sub(3).min(u16::MAX - 1);
                if app.scroll == u16::MAX {
                    app.scroll = 0;
                }
            }
        }
        MouseEventKind::ScrollDown => {
            if in_input {
                app.input_scroll = app.input_scroll.saturating_add(3);
            } else {
                app.scroll = app.scroll.saturating_add(3);
                app.follow_tail = false;
            }
        }
        MouseEventKind::Down(MouseButton::Left) => {
            if app
                .log_bar_area
                .is_some_and(|bar| point_in_rect(bar, mouse.column, mouse.row))
            {
                // Полоса прокрутки (0.35.1): клик по дорожке — переход к
                // позиции пропорционально высоте клика; ▲/▼ — шаг.
                scroll_log_to_ratio(app, mouse.row);
                app.focus = Focus::Log;
            } else if in_log {
                app.focus = Focus::Log;
            } else if in_input {
                app.focus = Focus::Input;
            }
        }
        MouseEventKind::Drag(MouseButton::Left)
            if app
                .log_bar_area
                .is_some_and(|bar| point_in_rect(bar, mouse.column, mouse.row)) =>
        {
            scroll_log_to_ratio(app, mouse.row);
        }
        _ => {}
    }
}

/// Позиция прокрутки по высоте клика/драга на полосе: верх дорожки —
/// начало журнала, низ — конец (пропорция по высоте полосы).
fn scroll_log_to_ratio(app: &mut App, row: u16) {
    let Some(bar) = app.log_bar_area else { return };
    if bar.height < 2 {
        return;
    }
    if row == bar.y {
        app.scroll = app.scroll.saturating_sub(1); // ▲ — шаг назад
    } else if row == bar.y + bar.height - 1 {
        app.scroll = app.scroll.saturating_add(1); // ▼ — шаг вперёд
    } else {
        let ratio = f64::from(row - bar.y) / f64::from(bar.height - 1);
        app.scroll = (ratio * app.log_max_scroll as f64) as u16;
    }
    app.follow_tail = false;
}

/// Точка внутри прямоугольника (hit-test мыши).
fn point_in_rect(area: Rect, x: u16, y: u16) -> bool {
    x >= area.x && x < area.x + area.width && y >= area.y && y < area.y + area.height
}

/// Буфер обмена — внешней утилитой, без новых зависимостей (репорт
/// 2026-08-09, /copy): Wayland — wl-copy, X11 — xclip/xsel, macOS —
/// pbcopy. Some(утилита) при успехе, None — говорящая строка локали
/// на стороне вызывающего (`sys_no_clipboard`).
fn copy_to_clipboard(text: &str) -> Option<&'static str> {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let candidates: [(&str, &[&str]); 4] = [
        ("wl-copy", &[]),
        ("xclip", &["-selection", "clipboard"]),
        ("xsel", &["--clipboard", "--input"]),
        ("pbcopy", &[]),
    ];
    for (prog, args) in candidates {
        let Ok(mut child) = Command::new(prog)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        else {
            continue;
        };
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(text.as_bytes());
        }
        if matches!(child.wait(), Ok(status) if status.success()) {
            return Some(prog);
        }
    }
    None
}

/// Байтовые сдвиги начал логических строк (разделитель — '\n').
fn line_starts(input: &str) -> Vec<usize> {
    let mut starts = vec![0];
    for (i, ch) in input.char_indices() {
        if ch == '\n' {
            starts.push(i + 1);
        }
    }
    starts
}

/// Позиция курсора (байты) как (логическая строка, колонка в символах).
fn cursor_row_col(input: &str, cursor: usize) -> (usize, usize) {
    let mut row = 0usize;
    let mut col = 0usize;
    for (i, ch) in input.char_indices() {
        if i >= cursor {
            break;
        }
        if ch == '\n' {
            row += 1;
            col = 0;
        } else {
            col += 1;
        }
    }
    (row, col)
}

/// Вертикальное движение курсора по многострочному буферу с
/// сохранением колонки (в символах; кириллица — 1 колонка, 2 байта).
fn move_cursor_vertical(app: &mut App, delta: isize) {
    let starts = line_starts(&app.input);
    let (row, col) = cursor_row_col(&app.input, app.cursor);
    let new_row = (row as isize + delta).clamp(0, starts.len() as isize - 1) as usize;
    let start = starts[new_row];
    let line_len = app.input[start..]
        .chars()
        .take_while(|&c| c != '\n')
        .count();
    let take = col.min(line_len);
    app.cursor = start
        + app.input[start..]
            .chars()
            .take(take)
            .map(char::len_utf8)
            .sum::<usize>();
}

/// Число экранных строк текста при переносе по ширине `width`
/// (перевод строки — новая строка; длинная строка переносится).
fn display_rows(text: &str, width: u16) -> u16 {
    let width = width.max(1) as usize;
    let mut rows = 1usize;
    let mut col = 0usize;
    for ch in text.chars() {
        if ch == '\n' {
            rows += 1;
            col = 0;
        } else {
            col += 1;
            if col >= width {
                rows += 1;
                col = 0;
            }
        }
    }
    rows as u16
}

/// Экранная позиция (колонка, строка) конца текста при переносе по
/// ширине — для установки курсора.
fn display_pos(text: &str, width: u16) -> (u16, u16) {
    let width = width.max(1) as usize;
    let mut row = 0usize;
    let mut col = 0usize;
    for ch in text.chars() {
        if ch == '\n' {
            row += 1;
            col = 0;
        } else {
            col += 1;
            if col >= width {
                row += 1;
                col = 0;
            }
        }
    }
    (col as u16, row as u16)
}

fn draw(frame: &mut Frame, app: &mut App) {
    // Многострочное поле ввода (репорт 2026-08-09): высота растёт с
    // числом экранных строк буфера, потолок — треть экрана (кап 10
    // строк), дальше поле крутится само (`input_scroll`). Ширина для
    // подсчёта переносов — полная: поле ввода занимает всю ширину
    // (инфо-панель живёт только в строке журнала).
    let full = frame.area();
    let rendered = format!(" › {}", app.input);
    let input_rows = display_rows(&rendered, full.width).max(1);
    let max_rows = (full.height / 3).clamp(1, 10);
    let input_height = input_rows.min(max_rows) + 1; // + бордер TOP
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4),            // шапка — фиксированная, не сдвигаемая
            Constraint::Min(3),               // журнал
            Constraint::Length(input_height), // ввод — растёт с буфером
            Constraint::Length(1),            // подсказки
        ])
        .split(full);
    draw_header(frame, app, chunks[0]);
    // §20.26: инфо-панель справа — только при достаточной ширине
    // (узкий терминал не теряет журнал: панель — чистый бонус).
    // Репорт 2026-08-16: при отпущенном захвате мыши (/mouse — режим
    // нативного выделения) панель НЕ рендерится, журнал занимает всю
    // ширину: иначе Shift+drag захватывал текст панели в выделение.
    if chunks[1].width >= 110 && app.mouse_capture {
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(76), Constraint::Percentage(24)])
            .split(chunks[1]);
        draw_log(frame, app, cols[0]);
        draw_side_panel(frame, app, cols[1]);
        app.log_area = cols[0];
    } else {
        draw_log(frame, app, chunks[1]);
        app.log_area = chunks[1];
    }
    app.input_area = chunks[2];
    draw_input(frame, app, chunks[2]);
    draw_hints(frame, app, chunks[3]);
    if app.slash_open {
        draw_slash_popup(frame, app, chunks[2]);
    }
    if let Some(picker) = &app.picker {
        draw_picker(frame, picker);
    }
    if let Some(Flow::AddAskKey { key_env, input, .. }) = &app.flow {
        draw_key_prompt(frame, key_env, input.len(), i18n::strings(app.locale));
    }
    if let Some(Flow::PluginAskRepo { input }) = &app.flow {
        draw_plugin_repo_prompt(frame, input, i18n::strings(app.locale));
    }
    if let Some(prompt) = &app.confirm_prompt {
        draw_confirm(
            frame,
            prompt,
            app.confirm_selection,
            i18n::strings(app.locale),
        );
    }
    if let Some(question) = &app.ask_prompt {
        draw_ask(frame, question, &app.ask_input, i18n::strings(app.locale));
    }
}

/// §20.26: инфо-панель сессии — данные только из App (кадр дешёвый,
/// обращений к журналу/сети на отрисовку нет).
fn draw_side_panel(frame: &mut Frame, app: &App, area: Rect) {
    let provider = app.active_provider.clone().unwrap_or_else(|| {
        app.config
            .providers
            .first()
            .map(|p| format!("{}:{}", p.name, p.model_id))
            .unwrap_or_else(|| "—".into())
    });
    let workspace = std::env::current_dir()
        .map(|p| {
            let s = p.display().to_string();
            if s.len() > 22 {
                format!("…{}", &s[s.len() - 21..])
            } else {
                s
            }
        })
        .unwrap_or_else(|_| ".".into());
    let grants = app.session_grants.lock().map(|g| g.len()).unwrap_or(0);
    let title_style = Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD);
    let dim = Style::default().fg(Color::DarkGray);
    let lines = vec![
        Line::from(Span::styled(" сессия", title_style)),
        Line::from(Span::styled(
            format!(" v{}", env!("CARGO_PKG_VERSION")),
            dim,
        )),
        Line::from(""),
        Line::from(format!(" {provider}")),
        Line::from(Span::styled(format!(" {workspace}"), dim)),
        Line::from(""),
        Line::from(format!(" ходов: {}", app.conversation.len() / 2)),
        Line::from(format!(" строк: {}", app.log.len())),
        Line::from(format!(" скилов: {}", app.skills.len())),
        Line::from(format!(" грантов: {grants}")),
    ];
    let block = Block::default()
        .borders(Borders::LEFT)
        .border_style(Style::default().fg(Color::DarkGray));
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

fn draw_confirm(frame: &mut Frame, prompt: &str, selection: usize, strings: &i18n::Strings) {
    let area = centered_rect(78, 30, frame.area());
    let mut lines: Vec<Line> = prompt
        .lines()
        .map(|l| Line::from(format!(" {l}")))
        .collect();
    lines.push(Line::from(""));
    // 0=да 1=сессия 2=проект 3=всё 4=нет; выбранный — инверсией.
    let options: [(&str, &str, Color); 5] = [
        (strings.confirm_yes, strings.confirm_yes_hint, Color::Green),
        (
            strings.confirm_session,
            strings.confirm_session_hint,
            Color::Cyan,
        ),
        (
            strings.confirm_project,
            strings.confirm_project_hint,
            Color::Cyan,
        ),
        (strings.confirm_all, strings.confirm_all_hint, Color::Cyan),
        (strings.confirm_no, strings.confirm_no_hint, Color::Red),
    ];
    let mut spans: Vec<Span> = Vec::new();
    for (i, (label, hint, color)) in options.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw(" "));
        }
        let style = if i == selection {
            Style::default()
                .fg(Color::Black)
                .bg(*color)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        spans.push(Span::styled(*label, style));
        if i == selection {
            spans.push(Span::styled(
                format!("({hint})"),
                Style::default().fg(Color::DarkGray),
            ));
        }
    }
    spans.push(Span::styled(
        strings.confirm_nav,
        Style::default().fg(Color::DarkGray),
    ));
    lines.push(Line::from(spans));
    let modal = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Yellow))
            .title(strings.confirm_title),
    );
    frame.render_widget(Clear, area);
    frame.render_widget(modal, area);
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
        let strings = i18n::strings(app.locale);
        format!(
            "{}{}",
            SPINNER[app.spinner_frame % SPINNER.len()],
            strings.header_thinking
        )
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
            Span::styled(
                i18n::strings(app.locale).header_workspace,
                Style::default().fg(Color::DarkGray),
            ),
            Span::raw(workspace),
        ]),
        Line::from(vec![
            Span::styled(
                i18n::strings(app.locale).header_models,
                Style::default().fg(Color::DarkGray),
            ),
            Span::raw(if models.is_empty() {
                i18n::strings(app.locale).header_models_empty.to_string()
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
/// **полужирный**, `код` (yellow). Блоки ```mermaid при успешном разборе
/// отрисовываются диаграммой (ROADMAP §20.26), при ошибке — как обычный
/// preformatted-блок (фолбэк незаметен для пользователя).
fn md_lines(text: &str) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let mut in_code = false;
    let mut code_lang = String::new();
    let mut mermaid_buf: Vec<String> = Vec::new();
    for raw in text.lines() {
        if raw.trim_start().starts_with("```") {
            if in_code {
                // Закрытие fenced-блока: накопленный mermaid — в отрисовку.
                if code_lang == "mermaid" {
                    lines.extend(mermaid_lines(&mermaid_buf.join("\n")));
                    mermaid_buf.clear();
                }
                code_lang.clear();
            } else {
                code_lang = raw.trim_start()[3..].trim().to_lowercase();
            }
            in_code = !in_code;
            continue;
        }
        if in_code {
            if code_lang == "mermaid" {
                mermaid_buf.push(raw.to_string());
                continue;
            }
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
    // Незакрытый fence: не теряем накопленный mermaid-блок.
    if in_code && code_lang == "mermaid" && !mermaid_buf.is_empty() {
        lines.extend(mermaid_lines(&mermaid_buf.join("\n")));
    }
    lines
}

/// Mermaid-fence → строки диаграммы; при ошибке разбора — исходный текст
/// блока как есть (тот же dim-стиль, что и у обычных fenced-блоков).
fn mermaid_lines(source: &str) -> Vec<Line<'static>> {
    match crate::tui_mermaid::render_source(source) {
        Ok(rendered) => rendered
            .into_iter()
            .map(|l| {
                Line::from(Span::styled(
                    format!("  {l}"),
                    Style::default().fg(Color::DarkGray),
                ))
            })
            .collect(),
        Err(_) => source
            .lines()
            .map(|l| {
                Line::from(Span::styled(
                    format!("  {l}"),
                    Style::default().fg(Color::DarkGray),
                ))
            })
            .collect(),
    }
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

fn draw_log(frame: &mut Frame, app: &mut App, area: Rect) {
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
    // Слайдер прокрутки (репорт 2026-08-16): видимый индикатор позиции —
    // текстовая колонка уступает правый столбец полосе, когда контент
    // длиннее экрана; иначе полоса не нужна и не рисуется.
    let (text_area, bar_area) = if total > height {
        let text = Rect {
            width: area.width.saturating_sub(1),
            ..area
        };
        let bar = Rect {
            x: area.x + area.width - 1,
            width: 1,
            ..area
        };
        (text, Some(bar))
    } else {
        (area, None)
    };
    let visible: Vec<Line> = lines.into_iter().skip(scroll).collect();
    let log = Paragraph::new(visible).wrap(Wrap { trim: false });
    frame.render_widget(log, text_area);
    if let Some(bar_area) = bar_area {
        let mut state = ScrollbarState::new(max_scroll).position(scroll);
        let bar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(Some("▲"))
            .end_symbol(Some("▼"))
            .thumb_style(Style::default().fg(Color::DarkGray));
        frame.render_stateful_widget(bar, bar_area, &mut state);
    }
    // Для мыши (0.35.1): область полосы и потолок прокрутки этого кадра.
    app.log_bar_area = bar_area;
    app.log_max_scroll = max_scroll;
}

fn draw_input(frame: &mut Frame, app: &mut App, area: Rect) {
    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(Style::default().fg(Color::DarkGray));
    // Многострочный буфер (репорт 2026-08-09): промпт « › » — часть
    // текста, чтобы переносы и курсор считались одной функцией
    // (display_pos). Окно прокрутки держит курсор видимым.
    let width = area.width.max(1);
    let visible = area.height.saturating_sub(1).max(1);
    let before_cursor = format!(" › {}", &app.input[..app.cursor]);
    let (cx, cy) = display_pos(&before_cursor, width);
    if cy < app.input_scroll {
        app.input_scroll = cy;
    } else if cy >= app.input_scroll + visible {
        app.input_scroll = cy + 1 - visible;
    }
    let prompt = Paragraph::new(Line::from(vec![
        Span::styled(
            " › ",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(app.input.clone()),
    ]))
    .block(block)
    .wrap(Wrap { trim: false })
    .scroll((app.input_scroll, 0));
    frame.render_widget(prompt, area);
    // Курсор на экране — в СИМВОЛАХ (кириллица — 1 колонка, 2 байта).
    frame.set_cursor_position((
        area.x + cx,
        area.y + 1 + cy.saturating_sub(app.input_scroll),
    ));
}

fn draw_hints(frame: &mut Frame, app: &App, area: Rect) {
    let strings = i18n::strings(app.locale);
    let hints = if app.confirm_prompt.is_some() {
        strings.hint_confirm
    } else if app.ask_prompt.is_some() {
        strings.ask_hint
    } else if app.busy {
        strings.hint_busy
    } else if app.picker.is_some() {
        strings.hint_picker
    } else if app.slash_open {
        strings.hint_slash
    } else if app.focus == Focus::Log {
        strings.hint_log_focus
    } else if !app.mouse_capture {
        strings.hint_mouse_off
    } else {
        strings.hint_default
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
    let strings = i18n::strings(app.locale);
    let items: Vec<ListItem> = filtered
        .iter()
        .map(|(name, about)| ListItem::new(format!("{name:<12} {}", about(strings))))
        .collect();
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(strings.slash_help),
        )
        .highlight_style(Style::default().bg(Color::DarkGray));
    let mut state = app.slash_state;
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
    let mut state = picker.state;
    frame.render_widget(Clear, area);
    frame.render_stateful_widget(list, area, &mut state);
}

fn draw_key_prompt(frame: &mut Frame, key_env: &str, input_len: usize, strings: &i18n::Strings) {
    let area = centered_rect(60, 20, frame.area());
    let prompt = Paragraph::new(vec![
        Line::from(format!("{} ({key_env}):", strings.key_label)),
        Line::from(format!(" {}", "*".repeat(input_len))),
        Line::from(Span::styled(
            strings.key_hint,
            Style::default().fg(Color::DarkGray),
        )),
    ])
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(strings.secret_title),
    );
    frame.render_widget(Clear, area);
    frame.render_widget(prompt, area);
}

/// /plugins add: ввод URL — не маскируется (не секрет).
fn draw_plugin_repo_prompt(frame: &mut Frame, input: &str, strings: &i18n::Strings) {
    let area = centered_rect(60, 20, frame.area());
    let prompt = Paragraph::new(vec![
        Line::from(format!(" {}", strings.plugin_repo_label)),
        Line::from(format!(" {input}")),
        Line::from(Span::styled(
            strings.plugin_repo_hint,
            Style::default().fg(Color::DarkGray),
        )),
    ])
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(strings.plugin_title),
    );
    frame.render_widget(Clear, area);
    frame.render_widget(prompt, area);
}

/// Модал human.ask (B7): вопрос агента со свободным вводом ответа.
fn draw_ask(frame: &mut Frame, question: &str, input: &str, strings: &i18n::Strings) {
    let area = centered_rect(70, 30, frame.area());
    let mut lines: Vec<Line> = question
        .lines()
        .map(|l| Line::from(format!(" {l}")))
        .collect();
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled(" › ", Style::default().fg(Color::Green)),
        Span::raw(input.to_string()),
    ]));
    lines.push(Line::from(Span::styled(
        strings.ask_hint,
        Style::default().fg(Color::DarkGray),
    )));
    let modal = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan))
            .title(strings.ask_title),
    );
    frame.render_widget(Clear, area);
    frame.render_widget(modal, area);
    // Курсор в конец поля ввода (в символах — кириллица 1 колонка).
    let cursor_x = area.x + 3 + input.chars().count() as u16;
    frame.set_cursor_position((
        cursor_x.min(area.x + area.width - 2),
        area.y + area.height - 3,
    ));
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
            active_provider: None,
            confirm_prompt: None,
            confirm_selection: 4,
            session_grants: std::sync::Arc::new(std::sync::Mutex::new(
                std::collections::HashSet::new(),
            )),
            skills: Vec::new(),
            agents: Vec::new(),
            pending_plugin_install: None,
            answer_tx: None,
            ask_prompt: None,
            ask_input: String::new(),
            ask_answer_tx: None,
            tx,
            rx,
            focus: Focus::Input,
            mouse_capture: true,
            locale: i18n::Locale::Ru,
            log_area: Rect::default(),
            log_bar_area: None,
            log_max_scroll: 0,
            input_area: Rect::default(),
            input_scroll: 0,
            done: false,
        };
        let names: Vec<&str> = app.slash_filtered().iter().map(|(n, _)| *n).collect();
        assert!(names.contains(&"/model"));
        assert!(names.contains(&"/models"));
        assert!(!names.contains(&"/help"));
        // Подменю (репорт 2026-08-12): «/config » показывает продолжение
        // «/config locale» (фильтр режет хвостовой пробел — в списке и
        // само «/config», это норма: два варианта продолжения).
        let mut app = app;
        app.input = "/config ".into();
        let names: Vec<&str> = app.slash_filtered().iter().map(|(n, _)| *n).collect();
        assert!(names.contains(&"/config locale"));
        assert!(names.contains(&"/config"));
    }

    /// §20.26: при ширине ≥ 110 рендерится инфо-панель, при узком —
    /// нет (журнал не теряет место).
    /// Мышь (репорт 2026-08-09): колесо крутит область под указателем —
    /// журнал с семантикой PageUp/Down, над полем ввода — окно ввода,
    /// журнал не трогается; клик переводит фокус.
    #[test]
    fn mouse_wheel_scrolls_log() {
        use crossterm::event::MouseEventKind;
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
            scroll: 100,
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
            active_provider: None,
            confirm_prompt: None,
            confirm_selection: 4,
            session_grants: std::sync::Arc::new(std::sync::Mutex::new(
                std::collections::HashSet::new(),
            )),
            skills: Vec::new(),
            agents: Vec::new(),
            pending_plugin_install: None,
            answer_tx: None,
            ask_prompt: None,
            ask_input: String::new(),
            ask_answer_tx: None,
            tx,
            rx,
            focus: Focus::Input,
            mouse_capture: true,
            locale: i18n::Locale::Ru,
            log_area: Rect::default(),
            log_bar_area: None,
            log_max_scroll: 0,
            input_area: Rect::default(),
            input_scroll: 0,
            done: false,
        };
        app.log_area = Rect::new(0, 4, 100, 20);
        app.input_area = Rect::new(0, 25, 100, 3);
        let at_log = |kind| MouseEvent {
            kind,
            column: 10,
            row: 10,
            modifiers: KeyModifiers::NONE,
        };
        handle_mouse(&mut app, at_log(MouseEventKind::ScrollUp));
        assert_eq!(app.scroll, 97);
        assert!(!app.follow_tail);
        handle_mouse(&mut app, at_log(MouseEventKind::ScrollDown));
        handle_mouse(&mut app, at_log(MouseEventKind::ScrollDown));
        assert_eq!(app.scroll, 103);
        // Колесо над полем ввода — крутится окно ввода, журнал на месте
        // (прежний баг репорта: колесо листало ИСТОРИЮ КОМАНД).
        let at_input = |kind| MouseEvent {
            kind,
            column: 10,
            row: 26,
            modifiers: KeyModifiers::NONE,
        };
        handle_mouse(&mut app, at_input(MouseEventKind::ScrollDown));
        assert_eq!(app.scroll, 103);
        assert_eq!(app.input_scroll, 3);
        handle_mouse(&mut app, at_input(MouseEventKind::ScrollUp));
        assert_eq!(app.input_scroll, 0);
        // Клик по журналу — фокус прокрутки; клик по вводу — обратно.
        handle_mouse(
            &mut app,
            at_log(MouseEventKind::Down(crossterm::event::MouseButton::Left)),
        );
        assert_eq!(app.focus, Focus::Log);
        handle_mouse(
            &mut app,
            at_input(MouseEventKind::Down(crossterm::event::MouseButton::Left)),
        );
        assert_eq!(app.focus, Focus::Input);
    }

    /// Фокус журнала: ↑↓ листают журнал, печать возвращает фокус и
    /// символ не теряется, Esc — просто назад в ввод.
    #[test]
    fn log_focus_arrows_scroll_and_typing_returns_focus() {
        let config = Config::default();
        let (tx, rx) = channel();
        let mut app = App {
            config,
            explicit_config: None,
            log: vec![],
            input: String::new(),
            cursor: 0,
            history: vec!["прежняя".into()],
            history_idx: None,
            conversation: vec![],
            scroll: 50,
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
            active_provider: None,
            confirm_prompt: None,
            confirm_selection: 4,
            session_grants: std::sync::Arc::new(std::sync::Mutex::new(
                std::collections::HashSet::new(),
            )),
            skills: Vec::new(),
            agents: Vec::new(),
            pending_plugin_install: None,
            answer_tx: None,
            ask_prompt: None,
            ask_input: String::new(),
            ask_answer_tx: None,
            tx,
            rx,
            focus: Focus::Log,
            mouse_capture: true,
            locale: i18n::Locale::Ru,
            log_area: Rect::default(),
            log_bar_area: None,
            log_max_scroll: 0,
            input_area: Rect::default(),
            input_scroll: 0,
            done: false,
        };
        // ↑ в фокусе журнала — прокрутка журнала, НЕ история команд.
        handle_key(&mut app, KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(app.scroll, 49);
        assert_eq!(app.history_idx, None);
        assert_eq!(app.focus, Focus::Log);
        // Печать возвращает фокус, символ доходит до поля ввода.
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('щ'), KeyModifiers::NONE),
        );
        assert_eq!(app.focus, Focus::Input);
        assert_eq!(app.input, "щ");
        // Esc в фокусе журнала — назад в ввод без побочек.
        app.focus = Focus::Log;
        handle_key(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(app.focus, Focus::Input);
        assert_eq!(app.input, "щ");
    }

    /// Многострочный ввод (репорт 2026-08-09): Alt+Enter — новая
    /// строка; вставка сохраняет переводы строк; ↑↓ двигают курсор по
    /// строкам и НЕ трогают историю команд.
    #[test]
    fn multiline_input_editing() {
        let config = Config::default();
        let (tx, rx) = channel();
        let mut app = App {
            config,
            explicit_config: None,
            log: vec![],
            input: String::new(),
            cursor: 0,
            history: vec!["старое".into()],
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
            active_provider: None,
            confirm_prompt: None,
            confirm_selection: 4,
            session_grants: std::sync::Arc::new(std::sync::Mutex::new(
                std::collections::HashSet::new(),
            )),
            skills: Vec::new(),
            agents: Vec::new(),
            pending_plugin_install: None,
            answer_tx: None,
            ask_prompt: None,
            ask_input: String::new(),
            ask_answer_tx: None,
            tx,
            rx,
            focus: Focus::Input,
            mouse_capture: true,
            locale: i18n::Locale::Ru,
            log_area: Rect::default(),
            log_bar_area: None,
            log_max_scroll: 0,
            input_area: Rect::default(),
            input_scroll: 0,
            done: false,
        };
        // Вставка с переводами строк — сохраняются (\r\n → \n).
        handle_paste(&mut app, "первая\r\nвторая\nтретья");
        assert_eq!(app.input, "первая\nвторая\nтретья");
        assert_eq!(app.cursor, app.input.len());
        // Alt+Enter — ещё одна строка.
        handle_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT));
        assert!(app.input.ends_with("\n"));
        // ↑↓ — по строкам буфера, история не тронута.
        handle_key(&mut app, KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(app.history_idx, None);
        let (row, _col) = cursor_row_col(&app.input, app.cursor);
        assert_eq!(row, 2); // строка «третья»
        handle_key(&mut app, KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        let (row, col) = cursor_row_col(&app.input, app.cursor);
        assert_eq!((row, col), (1, 0));
        // Вниз — обратно на строку «третья», колонка сохраняется.
        handle_key(&mut app, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        let (row, _col) = cursor_row_col(&app.input, app.cursor);
        assert_eq!(row, 2);
        // Курсор в конец короткой строки из длинной колонки — зажим.
        app.input = "длиннющая строка\nк\nхвост".into();
        app.cursor = 8; // колонка 8 первой строки
        move_cursor_vertical(&mut app, 1);
        let (row, col) = cursor_row_col(&app.input, app.cursor);
        assert_eq!((row, col), (1, 1)); // строка «к» — одна колонка
    }

    /// Однострочный ввод потока (ключ API/URL плагина) — вставка по-
    /// прежнему схлопывается в одну строку.
    #[test]
    fn paste_into_flow_input_stays_single_line() {
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
            flow: Some(Flow::PluginAskRepo {
                input: String::new(),
            }),
            pending_presets: vec![],
            staging_providers: vec![],
            staging_keys: vec![],
            active_provider: None,
            confirm_prompt: None,
            confirm_selection: 4,
            session_grants: std::sync::Arc::new(std::sync::Mutex::new(
                std::collections::HashSet::new(),
            )),
            skills: Vec::new(),
            agents: Vec::new(),
            pending_plugin_install: None,
            answer_tx: None,
            ask_prompt: None,
            ask_input: String::new(),
            ask_answer_tx: None,
            tx,
            rx,
            focus: Focus::Input,
            mouse_capture: true,
            locale: i18n::Locale::Ru,
            log_area: Rect::default(),
            log_bar_area: None,
            log_max_scroll: 0,
            input_area: Rect::default(),
            input_scroll: 0,
            done: false,
        };
        handle_paste(&mut app, "https://github.com/\nuser/repo");
        match &app.flow {
            Some(Flow::PluginAskRepo { input }) => {
                assert_eq!(input, "https://github.com/ user/repo");
            }
            _ => panic!("поток потерян"),
        }
    }

    /// display_rows/display_pos: переносы по ширине и переводы строк
    /// (кириллица — одна колонка).
    #[test]
    fn display_math_counts_wraps_and_newlines() {
        assert_eq!(display_rows(" › ", 10), 1);
        assert_eq!(display_rows(" › привет", 5), 2); // 9 символов по 5 колонок
        assert_eq!(display_rows(" › a\nb", 10), 2);
        assert_eq!(display_pos(" › a\nb", 10), (1, 1));
        assert_eq!(display_pos(" › привет", 5), (4, 1));
    }

    /// /mouse (репорт 2026-08-09): переключатель захвата — флаг
    /// двигается, sys-сообщения честные обе стороны. /copy на пустом
    /// журнале — говорящий отказ, не паника и не молчание.
    #[test]
    fn mouse_command_toggles_capture_and_copy_reports_empty_log() {
        let (tx, rx) = channel();
        let mut app = blank_app(tx, rx);
        assert!(app.mouse_capture);
        app.run_command("mouse");
        assert!(!app.mouse_capture);
        assert!(app.log.iter().any(|l| matches!(
            l,
            LogLine::Sys(t) if t.contains("мышь отпущена")
        )));
        app.run_command("mouse");
        assert!(app.mouse_capture);
        assert!(app.log.iter().any(|l| matches!(
            l,
            LogLine::Sys(t) if t.contains("мышь захвачена")
        )));
        app.run_command("copy");
        assert!(app.log.iter().any(|l| matches!(
            l,
            LogLine::Sys(t) if t.contains("нечего копировать")
        )));
    }

    /// Модал human.ask (B7): символы в буфер, Enter — Ok(строка) в
    /// канал и закрытие; Esc — Err(отмена). Канал НЕ забирается —
    /// вопросов за ход может быть несколько.
    #[test]
    fn ask_modal_input_enter_esc() {
        let (ask_tx, ask_rx) = channel::<Result<String, String>>();
        let (tx, rx) = channel();
        let mut app = blank_app(tx, rx);
        app.ask_answer_tx = Some(ask_tx);
        app.ask_prompt = Some("как звать?".to_string());
        for c in "вася".chars() {
            handle_key(
                &mut app,
                KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE),
            );
        }
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE),
        );
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('я'), KeyModifiers::NONE),
        );
        handle_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.ask_prompt.is_none());
        assert!(matches!(ask_rx.try_recv(), Ok(Ok(ref a)) if a == "вася"));
        assert!(
            app.ask_answer_tx.is_some(),
            "канал жив для следующего вопроса"
        );
        // Второй вопрос — Esc: отмена доезжает как ошибка asker'а.
        app.ask_prompt = Some("ещё вопрос".to_string());
        handle_key(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(matches!(ask_rx.try_recv(), Ok(Err(_))));
    }

    /// /config (2026-08-12, директива): меню параметров → пункт Locale
    /// → пикер из 8 локалей. /config locale xx — говорящий отказ БЕЗ
    /// смены. Запись в конфиг — seam `setup::set_locale_to` (покрыт
    /// своими тестами), здесь не дёргаем (не портить cwd тестов).
    #[test]
    fn config_menu_to_locale_picker_chain() {
        let (tx, rx) = channel();
        let mut app = blank_app(tx, rx);
        // /config — модалка параметров: показ конфигурации + Locale.
        app.run_command("config");
        assert!(matches!(app.flow, Some(Flow::ConfigMenu)));
        let picker = app.picker.as_ref().expect("меню параметров");
        assert_eq!(picker.items.len(), 2);
        assert!(picker.items[1].contains("Русский (ru)"));
        // Выбор «Locale» — модалка языка (8 локалей, текущая предвыбрана).
        app.picker.as_mut().unwrap().state.select(Some(1));
        app.advance_flow();
        assert!(matches!(app.flow, Some(Flow::LocalePick)));
        let picker = app.picker.as_ref().expect("пикер локалей");
        assert_eq!(picker.items.len(), 8);
        assert!(picker.items[1].contains("English (en)"));
        app.picker = None;
        app.flow = None;
        // Шорткат с мусорным кодом — отказ, локаль не тронута.
        app.run_command("config locale xx");
        assert!(app.log.iter().any(|l| matches!(
            l,
            LogLine::Sys(t) if t.contains("xx") && t.contains("доступны")
        )));
        assert_eq!(app.locale, i18n::Locale::Ru);
        // Пункт «показать конфигурацию» печатает строки с локалью.
        app.run_command("config");
        app.picker.as_mut().unwrap().state.select(Some(0));
        app.advance_flow();
        assert!(app.log.iter().any(|l| matches!(
            l,
            LogLine::Sys(t) if t.contains("Русский (ru)")
        )));
    }

    /// Общая фикстура App для рендер-тестов (панель/слайдер).
    fn test_app() -> App {
        let (tx, rx) = channel();
        App {
            config: Config::default(),
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
            active_provider: None,
            confirm_prompt: None,
            confirm_selection: 4,
            session_grants: std::sync::Arc::new(std::sync::Mutex::new(
                std::collections::HashSet::new(),
            )),
            skills: Vec::new(),
            agents: Vec::new(),
            pending_plugin_install: None,
            answer_tx: None,
            ask_prompt: None,
            ask_input: String::new(),
            ask_answer_tx: None,
            tx,
            rx,
            focus: Focus::Input,
            mouse_capture: true,
            locale: i18n::Locale::Ru,
            log_area: Rect::default(),
            log_bar_area: None,
            log_max_scroll: 0,
            input_area: Rect::default(),
            input_scroll: 0,
            done: false,
        }
    }

    #[test]
    fn side_panel_renders_only_when_wide() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let mut app = test_app();

        let wide = TestBackend::new(140, 30);
        let mut terminal = Terminal::new(wide).expect("terminal");
        terminal.draw(|f| draw(f, &mut app)).expect("draw wide");
        let content: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(content.contains("сессия"), "панель при 140 колонках");

        let narrow = TestBackend::new(90, 30);
        let mut terminal = Terminal::new(narrow).expect("terminal");
        terminal.draw(|f| draw(f, &mut app)).expect("draw narrow");
        let content: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(!content.contains("сессия"), "без панели при 90 колонках");

        // Репорт 2026-08-16: режим выделения (захват отпущен, /mouse)
        // прячет панель даже на широком терминале — нативное выделение
        // покрывает только журнал, не текст панели.
        app.mouse_capture = false;
        let wide = TestBackend::new(140, 30);
        let mut terminal = Terminal::new(wide).expect("terminal");
        terminal.draw(|f| draw(f, &mut app)).expect("draw wide");
        let content: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(!content.contains("сессия"), "без панели в режиме выделения");
    }

    /// Слайдер прокрутки журнала (репорт 2026-08-16): контент длиннее
    /// экрана — полоса с ▲/▼ справа от текста; короткий журнал — без неё.
    #[test]
    fn log_scrollbar_appears_only_when_content_overflows() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let mut app = test_app();
        for i in 0..100 {
            app.log.push(LogLine::Sys(format!("строка {i}")));
        }
        let wide = TestBackend::new(100, 20);
        let mut terminal = Terminal::new(wide).expect("terminal");
        terminal.draw(|f| draw(f, &mut app)).expect("draw");
        let content: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(content.contains('▲'), "полоса прокрутки при переполнении");

        let mut app = test_app();
        app.log.push(LogLine::Sys("коротко".into()));
        let wide = TestBackend::new(100, 20);
        let mut terminal = Terminal::new(wide).expect("terminal");
        terminal.draw(|f| draw(f, &mut app)).expect("draw");
        let content: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(!content.contains('▲'), "без полосы на коротком журнале");
    }

    /// Полоса прокрутки интерактивна (0.35.1): клик по середине дорожки
    /// — позиция пропорционально; ▲/▼ — шаг; драг — та же пропорция.
    #[test]
    fn scrollbar_track_click_and_arrows_scroll_the_log() {
        let mut app = test_app();
        app.log_bar_area = Some(Rect {
            x: 99,
            y: 4,
            width: 1,
            height: 22,
        });
        app.log_max_scroll = 100;
        // Середина дорожки (row 4+11 из 21 интервала) — примерно половина.
        scroll_log_to_ratio(&mut app, 15);
        assert!((45..=60).contains(&app.scroll), "середина: {}", app.scroll);
        assert!(!app.follow_tail);
        // ▲/▼ — шаги.
        let before = app.scroll;
        scroll_log_to_ratio(&mut app, 4);
        assert_eq!(app.scroll, before - 1, "▲ — шаг назад");
        scroll_log_to_ratio(&mut app, 4 + 21);
        assert_eq!(app.scroll, before, "▼ — шаг вперёд");
        // Без полосы — no-op.
        app.log_bar_area = None;
        scroll_log_to_ratio(&mut app, 10);
        assert_eq!(app.scroll, before);
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

    /// ROADMAP §20.26: ```mermaid fence в ответе модели отрисовывается
    /// диаграммой; при ошибке разбора — сырой блок без изменений.
    #[test]
    fn md_lines_renders_mermaid_fence() {
        let lines = md_lines("текст\n```mermaid\ngraph LR\nA[Старт] --> B[Финиш]\n```\nпосле");
        let plain: String = lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.to_string())
                    .collect::<String>()
            })
            .collect();
        assert!(plain.contains('▶'), "диаграмма отрисована: {plain}");
        assert!(plain.contains("Старт"), "метка узла: {plain}");
        assert!(!plain.contains("graph LR"), "сырой исходник скрыт: {plain}");
        assert!(
            plain.contains("после"),
            "текст после блока на месте: {plain}"
        );
    }

    #[test]
    fn md_lines_mermaid_fallback_keeps_raw_block() {
        // Битый mermaid: пользователь видит исходник, как раньше.
        let lines = md_lines("```mermaid\nэто не диаграмма\n```");
        let plain: String = lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.to_string())
                    .collect::<String>()
            })
            .collect();
        assert!(
            plain.contains("это не диаграмма"),
            "фолбэк на исходник: {plain}"
        );
    }

    /// /skills в TUI: ветка run_command выводит установленные скиллы.
    #[test]
    fn run_command_skills_lists_installed() {
        let (tx, rx) = channel();
        let mut app = App {
            config: Config::default(),
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
            active_provider: None,
            confirm_prompt: None,
            confirm_selection: 4,
            session_grants: std::sync::Arc::new(std::sync::Mutex::new(
                std::collections::HashSet::new(),
            )),
            skills: vec![crate::skills::Skill {
                name: "code-review-ru".into(),
                version: "0.1.0".into(),
                description: "Ревью".into(),
                triggers: vec!["/review".into()],
                tools: vec![],
                permissions: vec![],
                model_tier: None,
                body: String::new(),
                origin: std::path::PathBuf::from("/tmp/x"),
            }],
            agents: Vec::new(),
            pending_plugin_install: None,
            answer_tx: None,
            ask_prompt: None,
            ask_input: String::new(),
            ask_answer_tx: None,
            tx,
            rx,
            focus: Focus::Input,
            mouse_capture: true,
            locale: i18n::Locale::Ru,
            log_area: Rect::default(),
            log_bar_area: None,
            log_max_scroll: 0,
            input_area: Rect::default(),
            input_scroll: 0,
            done: false,
        };
        app.run_command("skills");
        let text = app
            .log
            .iter()
            .map(|l| match l {
                LogLine::Sys(t) | LogLine::Tool(t) | LogLine::User(t) => t.clone(),
                LogLine::Assistant(t) | LogLine::Err(t) => t.clone(),
            })
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            text.contains("code-review-ru"),
            "ожидали скилл в логе: {text}"
        );
    }

    /// /agents в TUI (§20.36 — команды не было вовсе): ветка run_command
    /// выводит установленных субагентов, тот же приём, что /skills.
    #[test]
    fn run_command_agents_lists_installed() {
        let (tx, rx) = channel();
        let mut app = App {
            config: Config::default(),
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
            active_provider: None,
            confirm_prompt: None,
            confirm_selection: 4,
            session_grants: std::sync::Arc::new(std::sync::Mutex::new(
                std::collections::HashSet::new(),
            )),
            skills: vec![],
            agents: vec![crate::agents::AgentDef {
                name: "reviewer".into(),
                description: "Только чтение, ревью diff'ов".into(),
                ..Default::default()
            }],
            pending_plugin_install: None,
            answer_tx: None,
            ask_prompt: None,
            ask_input: String::new(),
            ask_answer_tx: None,
            tx,
            rx,
            focus: Focus::Input,
            mouse_capture: true,
            locale: i18n::Locale::Ru,
            log_area: Rect::default(),
            log_bar_area: None,
            log_max_scroll: 0,
            input_area: Rect::default(),
            input_scroll: 0,
            done: false,
        };
        app.run_command("agents");
        let text = app
            .log
            .iter()
            .map(|l| match l {
                LogLine::Sys(t) | LogLine::Tool(t) | LogLine::User(t) => t.clone(),
                LogLine::Assistant(t) | LogLine::Err(t) => t.clone(),
            })
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            text.contains("reviewer"),
            "ожидали субагента в логе: {text}"
        );
    }

    /// §20.36: ввод URL плагина — Enter с непустым текстом сигналит
    /// `pending_plugin_install` (реальный запуск — в `event_loop`, не
    /// здесь: нужен `Terminal` для приостановки TUI). Esc/Backspace/
    /// Char — та же механика, что у AddAskKey.
    #[test]
    fn plugin_ask_repo_enter_sets_pending_install() {
        let (tx, rx) = channel();
        let mut app = App {
            config: Config::default(),
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
            flow: Some(Flow::PluginAskRepo {
                input: String::new(),
            }),
            pending_presets: vec![],
            staging_providers: vec![],
            staging_keys: vec![],
            active_provider: None,
            confirm_prompt: None,
            confirm_selection: 4,
            session_grants: std::sync::Arc::new(std::sync::Mutex::new(
                std::collections::HashSet::new(),
            )),
            skills: vec![],
            agents: vec![],
            pending_plugin_install: None,
            answer_tx: None,
            ask_prompt: None,
            ask_input: String::new(),
            ask_answer_tx: None,
            tx,
            rx,
            focus: Focus::Input,
            mouse_capture: true,
            locale: i18n::Locale::Ru,
            log_area: Rect::default(),
            log_bar_area: None,
            log_max_scroll: 0,
            input_area: Rect::default(),
            input_scroll: 0,
            done: false,
        };
        for c in "https://example.test/plugin".chars() {
            handle_key(
                &mut app,
                KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE),
            );
        }
        assert!(app.pending_plugin_install.is_none(), "ещё не Enter");
        handle_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(
            app.pending_plugin_install.as_deref(),
            Some("https://example.test/plugin")
        );
        assert!(app.flow.is_none(), "flow снят после Enter");
    }

    /// Esc отменяет ввод URL без сигнала установки.
    #[test]
    fn plugin_ask_repo_esc_cancels_without_pending_install() {
        let (tx, rx) = channel();
        let mut app = App {
            config: Config::default(),
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
            flow: Some(Flow::PluginAskRepo {
                input: "partial".into(),
            }),
            pending_presets: vec![],
            staging_providers: vec![],
            staging_keys: vec![],
            active_provider: None,
            confirm_prompt: None,
            confirm_selection: 4,
            session_grants: std::sync::Arc::new(std::sync::Mutex::new(
                std::collections::HashSet::new(),
            )),
            skills: vec![],
            agents: vec![],
            pending_plugin_install: None,
            answer_tx: None,
            ask_prompt: None,
            ask_input: String::new(),
            ask_answer_tx: None,
            tx,
            rx,
            focus: Focus::Input,
            mouse_capture: true,
            locale: i18n::Locale::Ru,
            log_area: Rect::default(),
            log_bar_area: None,
            log_max_scroll: 0,
            input_area: Rect::default(),
            input_scroll: 0,
            done: false,
        };
        handle_key(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.pending_plugin_install.is_none());
        assert!(app.flow.is_none());
    }

    fn blank_app(tx: Sender<WorkerMsg>, rx: Receiver<WorkerMsg>) -> App {
        App {
            config: Config::default(),
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
            active_provider: None,
            confirm_prompt: None,
            confirm_selection: 4,
            session_grants: std::sync::Arc::new(std::sync::Mutex::new(
                std::collections::HashSet::new(),
            )),
            skills: vec![],
            agents: vec![],
            pending_plugin_install: None,
            answer_tx: None,
            ask_prompt: None,
            ask_input: String::new(),
            ask_answer_tx: None,
            tx,
            rx,
            focus: Focus::Input,
            mouse_capture: true,
            locale: i18n::Locale::Ru,
            log_area: Rect::default(),
            log_bar_area: None,
            log_max_scroll: 0,
            input_area: Rect::default(),
            input_scroll: 0,
            done: false,
        }
    }

    /// Репорт 2026-08-09: вставка многострочного текста БЕЗ bracketed
    /// paste шлётся терминалом как синтетический Enter на каждый \n —
    /// `submit()` без этой защиты отправлял бы каждую строку отдельным
    /// сообщением. Событие `Event::Paste` приходит одним куском —
    /// `handle_paste` сохраняет переводы строк (поле ввода
    /// многострочное с того же дня), не зовя submit.
    #[test]
    fn handle_paste_inserts_as_single_normalized_message_without_submitting() {
        let (tx, rx) = channel();
        let mut app = blank_app(tx, rx);

        handle_paste(&mut app, "## Раздел 1\n\n### Раздел 2\nтело раздела");

        assert_eq!(app.input, "## Раздел 1\n\n### Раздел 2\nтело раздела");
        assert_eq!(app.cursor, app.input.len());
        assert!(
            app.conversation.is_empty(),
            "вставка не должна сама отправлять сообщение"
        );
    }

    /// Вставка при открытом текстовом вводе (ключ/URL плагина) идёт в
    /// НЕГО, не в поле сообщения — тот же приоритет, что у handle_key.
    #[test]
    fn handle_paste_goes_into_active_text_flow_not_message_input() {
        let (tx, rx) = channel();
        let mut app = blank_app(tx, rx);
        app.flow = Some(Flow::PluginAskRepo {
            input: "https://".into(),
        });

        handle_paste(&mut app, "example.test/plugin");

        assert!(matches!(
            &app.flow,
            Some(Flow::PluginAskRepo { input }) if input == "https://example.test/plugin"
        ));
        assert!(app.input.is_empty());
    }

    /// Пикер открыт — вставлять некуда, молча игнорируем (не паникуем,
    /// не портим состояние пикера).
    #[test]
    fn handle_paste_ignored_while_picker_open() {
        let (tx, rx) = channel();
        let mut app = blank_app(tx, rx);
        app.picker = Some(Picker::new("test", vec!["a".into()], false));

        handle_paste(&mut app, "не должно никуда попасть");

        assert!(app.input.is_empty());
    }

    /// Репорт 2026-08-09 (вторая половина находки): без этой защиты
    /// быстрый повторный Enter (в т.ч. синтетический — от вставки без
    /// bracketed paste) запускал ВТОРОЙ воркер поверх первого; когда
    /// ответы приходили вперемешку, `chat_history::append` записывал
    /// ОТВЕТ МОДЕЛИ как будто это ввод пользователя (зеркалирование
    /// ленты, репорт «свободный цикл исчерпал лимит ходов без Finish»).
    #[test]
    fn submit_is_a_noop_while_busy() {
        let (tx, rx) = channel();
        let mut app = blank_app(tx, rx);
        app.busy = true;
        app.input = "второе сообщение поверх первого хода".into();

        app.submit();

        assert!(
            app.conversation.is_empty(),
            "занятый воркер не должен получать второе сообщение"
        );
        assert_eq!(
            app.input, "второе сообщение поверх первого хода",
            "ввод не должен очищаться, пока не отправлен"
        );
    }

    /// Репорт 2026-08-09: «инструменты не обновляются, настроек нет» —
    /// в TUI не было способа посмотреть список доступных инструментов
    /// вообще (только `/config`, 3 строки, без списка). `/tools`
    /// переиспользует ту же функцию, что видит модель (`chat::
    /// tools_catalog`) — что в логе, то и реально доступно агенту.
    #[test]
    fn run_command_tools_lists_builtin_and_mcp() {
        let (tx, rx) = channel();
        let mut config = Config::default();
        config.mcp_servers.push(crate::config::McpServerConfig {
            name: "demo-mcp".into(),
            command: "demo".into(),
            args: vec![],
        });
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
            active_provider: None,
            confirm_prompt: None,
            confirm_selection: 4,
            session_grants: std::sync::Arc::new(std::sync::Mutex::new(
                std::collections::HashSet::new(),
            )),
            skills: vec![],
            agents: Vec::new(),
            pending_plugin_install: None,
            answer_tx: None,
            ask_prompt: None,
            ask_input: String::new(),
            ask_answer_tx: None,
            tx,
            rx,
            focus: Focus::Input,
            mouse_capture: true,
            locale: i18n::Locale::Ru,
            log_area: Rect::default(),
            log_bar_area: None,
            log_max_scroll: 0,
            input_area: Rect::default(),
            input_scroll: 0,
            done: false,
        };
        app.run_command("tools");
        let text = app
            .log
            .iter()
            .map(|l| match l {
                LogLine::Sys(t) | LogLine::Tool(t) | LogLine::User(t) => t.clone(),
                LogLine::Assistant(t) | LogLine::Err(t) => t.clone(),
            })
            .collect::<Vec<_>>()
            .join(" ");
        assert!(text.contains("files.write"), "встроенный: {text}");
        assert!(text.contains("terminal.exec"), "встроенный: {text}");
        assert!(text.contains("demo-mcp.*"), "MCP-сервер: {text}");
    }

    /// `/tools`/`/skills` реально работают в run_command (проверено выше),
    /// но должны быть и в автодополнении/`/help` — иначе не обнаружимы.
    #[test]
    fn slash_commands_advertise_tools_and_skills() {
        let names: Vec<&str> = SLASH_COMMANDS.iter().map(|(name, _)| *name).collect();
        assert!(names.contains(&"/tools"));
        assert!(names.contains(&"/skills"));
    }

    /// §20.36: установка/удаление скилов, субагентов, плагинов из TUI —
    /// без объявления в SLASH_COMMANDS они не видны ни в /help, ни в
    /// автодополнении по `/` (тот же класс пробела, что был у /skills
    /// до §20.33).
    #[test]
    fn slash_commands_advertise_ext_and_plugin_actions() {
        let names: Vec<&str> = SLASH_COMMANDS.iter().map(|(name, _)| *name).collect();
        for expected in [
            "/skills add",
            "/skills remove",
            "/agents",
            "/agents add",
            "/agents remove",
            "/plugins",
            "/plugins add",
            "/plugins remove",
        ] {
            assert!(names.contains(&expected), "нет команды {expected}");
        }
    }

    /// Репорт 0.14.0: пользователь жмёт ↓ к «проекту», модал слушал
    /// только ←→ — подсветка не двигалась, Enter на «нет» давал отказ
    /// при намерении разрешить. Теперь обе оси двигают выбор.
    #[test]
    fn confirm_modal_arrows_both_axes_and_enter() {
        let (answer_tx, answer_rx) = channel();
        let (tx, rx) = channel();
        let mut app = App {
            config: Config::default(),
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
            active_provider: None,
            confirm_prompt: Some("test".into()),
            confirm_selection: 4,
            session_grants: std::sync::Arc::new(std::sync::Mutex::new(
                std::collections::HashSet::new(),
            )),
            skills: Vec::new(),
            agents: Vec::new(),
            pending_plugin_install: None,
            answer_tx: Some(answer_tx),
            ask_prompt: None,
            ask_input: String::new(),
            ask_answer_tx: None,
            tx,
            rx,
            focus: Focus::Input,
            mouse_capture: true,
            locale: i18n::Locale::Ru,
            log_area: Rect::default(),
            log_bar_area: None,
            log_max_scroll: 0,
            input_area: Rect::default(),
            input_scroll: 0,
            done: false,
        };
        // ↓ ↓ ↓ от «нет» (4) → да(0) → сессия(1) → проект(2); Enter.
        for _ in 0..3 {
            handle_key(&mut app, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        }
        assert_eq!(app.confirm_selection, 2);
        handle_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(answer_rx.try_recv(), Ok(ConfirmAnswer::Project)));
        assert!(app.confirm_prompt.is_none());
        // ↑ от «нет» — «всё для проекта» (3); Enter → ProjectAll.
        app.confirm_prompt = Some("test2".into());
        app.confirm_selection = 4;
        let (answer_tx2, answer_rx2) = channel();
        app.answer_tx = Some(answer_tx2);
        handle_key(&mut app, KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(app.confirm_selection, 3);
        handle_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(
            answer_rx2.try_recv(),
            Ok(ConfirmAnswer::ProjectAll)
        ));
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
            active_provider: None,
            confirm_prompt: None,
            confirm_selection: 4,
            session_grants: std::sync::Arc::new(std::sync::Mutex::new(
                std::collections::HashSet::new(),
            )),
            skills: Vec::new(),
            agents: Vec::new(),
            pending_plugin_install: None,
            answer_tx: None,
            ask_prompt: None,
            ask_input: String::new(),
            ask_answer_tx: None,
            tx,
            rx,
            focus: Focus::Input,
            mouse_capture: true,
            locale: i18n::Locale::Ru,
            log_area: Rect::default(),
            log_bar_area: None,
            log_max_scroll: 0,
            input_area: Rect::default(),
            input_scroll: 0,
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
