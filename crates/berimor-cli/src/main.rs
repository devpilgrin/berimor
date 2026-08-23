//! `berimor` — точка входа: CLI, загрузка конфигурации, оркестрация подкоманд.
//!
//! Источник: `docs/arch/stack.md` §2, `docs/arch/deployment.md` §5. Подкоманда `verify`
//! существует потому, что bootstrap-слой (npm, TypeScript) не дублирует
//! криптографическую проверку на другом языке — вызывает эту же подкоманду
//! (ADR-0025). ROADMAP: F3.

use clap::{Parser, Subcommand};
use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::ExitCode;

mod agent_dispatch;
mod agents;
mod builtin_dispatch;
mod builtin_edit;
mod builtin_human;
mod builtin_memory;
mod builtin_search;
mod builtin_sessions_search;
mod builtin_snapshots;
mod builtin_terminal_bg;
mod builtin_todo;
mod builtin_vcs;
mod builtin_websearch;
mod catalog;
mod chat;
mod chat_compaction;
mod chat_history;
mod chat_tui;
mod chat_ui;
mod config;
mod daemon;
mod ext_cmd;
mod i18n;
mod landlock;
mod mcp_dispatch;
mod mcp_serve;
mod memory;
mod metering;
mod oauth;
mod observe;
mod plugin_install;
mod plugin_runtime;
mod presets;
mod rules;
mod run;
mod self_update;
mod serve;
mod sessions;
mod setup;
mod skill_lint;
mod skill_review;
mod skills;
mod trust;
mod tui_mermaid;
mod verify;

#[derive(Parser)]
#[command(
    name = "berimor",
    version,
    about = "Детерминированное ядро агентной системы"
)]
struct Cli {
    /// Путь к файлу конфигурации. По умолчанию — `.berimor/config.toml`
    /// (легаси `./berimor.toml`, если уже существует, — используется он),
    /// отсутствие файла не ошибка (используются значения по умолчанию).
    #[arg(long, global = true)]
    config: Option<PathBuf>,

    /// Без подкоманды — интерактивный чат (§20.13): `berimor` ==
    /// `berimor chat`. Директива пользователя: CLI агента по умолчанию
    /// разговаривает, а не требует команду.
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Интерактивный диалог с агентом (свободный цикл со встроенными
    /// инструментами; рабочая область — текущая директория).
    Chat,
    /// Мастер первичной настройки: пресеты провайдеров в глобальный
    /// конфиг + ключи в secrets.env (§20.12).
    Setup,
    /// Выполнить процесс (структурированную задачу).
    Run {
        /// Путь к декларации процесса (YAML).
        process: String,
        /// Продолжить существующий инстанс по его идентификатору
        /// (восстановление из журнала, P3).
        #[arg(long)]
        resume: Option<String>,
        /// Вход процесса — JSON-объект начального состояния.
        #[arg(long)]
        input: Option<String>,
        /// BR-05 (полевой тест 2026-08-14): неинтерактивный режим —
        /// запрос подтверждения трактуется как отказ с диагностикой
        /// (для скриптов/демона без терминала; то же даёт переменная
        /// окружения BERIMOR_NON_INTERACTIVE=1).
        #[arg(long)]
        non_interactive: bool,
    },
    /// Проверить подпись артефакта — вызывается bootstrap-слоем (ADR-0025).
    Verify { artifact: String },
    /// Прогнать процесс `agent-self-update` (ADR-0019).
    SelfUpdate {
        /// Продолжить существующий инстанс self-update по его
        /// идентификатору (восстановление из журнала, тот же смысл, что
        /// у `Command::Run`'s `--resume`).
        #[arg(long)]
        resume: Option<String>,
    },
    /// Установить плагин из доверенного репозитория.
    Plugin {
        #[command(subcommand)]
        action: PluginAction,
    },
    /// Расписания процессов: добавление, список, снятие (§20.22).
    Schedule {
        #[command(subcommand)]
        action: ScheduleAction,
    },
    /// Живые сессии хоста — реестр на общем журнале (§20.22 v2).
    Sessions,
    /// HTTP-сервис поверх run/schedule/sessions (prompt-next-wave.md
    /// задача 2). Требует `[serve] token_env` в конфиге — без него не
    /// стартует (I2, не анонимный доступ к исполнению процессов).
    Serve {
        /// Переопределить порт из конфига.
        #[arg(long)]
        port: Option<u16>,
    },
    /// MCP-сервер (stdio, 0.37.0): внешние агенты гоняют процессы
    /// berimor как детерминированный контур — process.list/run,
    /// trace.read.
    McpServe,
    /// Память: консолидация семантических дублей (prompt-next-wave.md
    /// задача 3). Требует `[memory] embeddings = true` и сборки с
    /// `--features embeddings`.
    Memory {
        #[command(subcommand)]
        action: MemoryAction,
    },
    /// Демон расписаний: исполняет due-процессы тик за тиком (§20.22).
    Daemon {
        /// Один тик и выход (для cron и ручного запуска).
        #[arg(long)]
        once: bool,
        /// Потолок сна между тиками, мс (по умолчанию 60000).
        #[arg(long, default_value = "60000")]
        tick_cap: i64,
    },
    /// Скилы: список (установленные и доступные в каталоге), установка,
    /// удаление (§20.16).
    Skill {
        #[command(subcommand)]
        action: ext_cmd::ExtAction,
    },
    /// Субагенты: список, установка, удаление (§20.16; исполнитель —
    /// отдельный этап ROADMAP).
    Agent {
        #[command(subcommand)]
        action: ext_cmd::ExtAction,
    },
    /// Доверенный список репозиториев (ROADMAP D5) — источник обновлений
    /// (`self-update`) и плагинов (`plugin install`).
    Trust {
        #[command(subcommand)]
        action: TrustAction,
    },
    /// Разобранная конфигурация — без этого нельзя проверить загрузку
    /// снаружи юнит-тестов.
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
    /// OAuth-вход по подписке (PKCE, ADR-0027): Claude Pro/Max, ChatGPT
    /// Plus/Pro — без API-ключа. Токены — в secrets.env (0600), refresh —
    /// прозрачно кодом (§20.25).
    Login {
        /// Провайдер: claude | openai.
        #[arg(long)]
        provider: Option<String>,
        /// Ручной ввод кода (headless) вместо loopback-listener.
        #[arg(long)]
        manual: bool,
        /// Показать сохранённые OAuth-профили (без значений токенов, I4).
        #[arg(long)]
        list: bool,
    },
    /// Отзыв OAuth-профиля: удаление записи из реестра секретов (§20.25).
    Logout {
        /// Провайдер: claude | openai.
        #[arg(long)]
        provider: String,
    },
    /// Человекочитаемая трассировка журнала одного инстанса (O1).
    Trace {
        /// Идентификатор инстанса (тот же, что печатает `run`/`--resume`).
        instance: String,
    },
    /// Стоимость прогона: токены и деньги по шагам (волна A, 0.38.0).
    Cost {
        /// Идентификатор инстанса.
        instance: String,
    },
    /// Офлайн-прогон золотого набора: доля веток, доля отказов Mediation (O2).
    Eval {
        /// Директория с `process.yaml` и `<сценарий>.json` файлами входа.
        golden_dir: PathBuf,
    },
}

#[derive(Subcommand)]
enum MemoryAction {
    /// Найти и слить семантически близкие дубли фактов (порог 0.75).
    /// Ничего не удаляется молча — каждое слияние журналируется
    /// событием `FactsConsolidated` (`berimor trace`).
    Consolidate,
}

#[derive(Subcommand)]
enum ScheduleAction {
    /// Добавить расписание: --every <dur> (повторяющееся) или
    /// --once-in <dur> (одноразовое); длительность с суффиксом (30s/10m/1h).
    Add {
        /// Путь к декларации процесса (YAML).
        process: String,
        /// Повторяющееся расписание с интервалом.
        #[arg(long)]
        every: Option<String>,
        /// Одноразовое срабатывание через интервал.
        #[arg(long)]
        once_in: Option<String>,
        /// Вход процесса — JSON-объект начального состояния.
        #[arg(long)]
        input: Option<String>,
    },
    /// Список расписаний по ближайшему срабатыванию.
    List,
    /// Снять расписание по идентификатору.
    Remove { id: String },
}

#[derive(Subcommand)]
enum PluginAction {
    /// Установить плагин из локального каталога или git-репозитория БЕЗ
    /// проверки подписи (явный --allow-unsigned; доверенный путь — install).
    InstallLocal {
        /// Путь к каталогу (бинарник + manifest.yaml) или git-URL.
        source: String,
        /// Осознанное согласие на установку без подписи.
        #[arg(long)]
        allow_unsigned: bool,
    },
    Install {
        repo: String,
        /// Продолжить существующий инстанс установки по его
        /// идентификатору (восстановление из журнала).
        #[arg(long)]
        resume: Option<String>,
        /// Ожидаемая идентичность подписанта (SAN-префикс пути к
        /// workflow-файлу) — обязательна, если репозиторий ещё не в
        /// доверенном списке (TOFU, первая установка).
        #[arg(long)]
        signer_workflow: Option<String>,
        /// Паттерн разрешённых тегов, по умолчанию "v*.*.*" — учитывается
        /// только при первой установке из нового репозитория.
        #[arg(long)]
        allowed_ref: Option<String>,
        /// Потолок ACL через запятую, например `net.http,fs.read` —
        /// учитывается только при первой установке из нового репозитория.
        #[arg(long)]
        capability_ceiling: Option<String>,
    },
    /// Удалить установленный плагин (§20.36 — не трогает доверенный
    /// список: репозиторий мог использоваться другими плагинами).
    Remove { name: String },
}

#[derive(Subcommand)]
enum TrustAction {
    /// Добавить репозиторий в доверенный список — показывает предлагаемую
    /// запись и требует подтверждения (I2), прежде чем что-либо
    /// записывается в журнал.
    Add {
        repo: String,
        #[arg(long, default_value = "v*.*.*")]
        allowed_ref: String,
        /// SAN-префикс идентичности подписанта — привязка к CI-workflow
        /// репозитория (тот же формат, что `verify.rs::ReleaseWorkflowPath`
        /// использует для собственного релиза berimor).
        #[arg(long)]
        signer_workflow: String,
        /// Через запятую, например `net.http,fs.read`.
        #[arg(long, default_value = "")]
        capability_ceiling: String,
    },
    /// Удалить репозиторий из доверенного списка — тоже требует
    /// подтверждения.
    Remove { repo: String },
    /// Напечатать текущий доверенный список.
    List,
}

#[derive(Subcommand)]
enum ConfigAction {
    /// Напечатать конфигурацию, которая реально будет использована.
    Show,
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    // First-run (§20.12): ни глобального, ни локального конфига нет, а
    // команда интерактивна и требует провайдера — предлагаем мастер
    // сразу, а не отсылку в документацию. Не-терминал (скрипты, пайпы)
    // мастер не предлагает — только факт отсутствия конфигурации.
    let needs_provider = matches!(
        cli.command.as_ref().unwrap_or(&Command::Chat),
        Command::Chat | Command::Run { .. }
    );
    if needs_provider
        && !config::any_config_present(cli.config.as_deref().map(PathBuf::from).as_deref())
        && std::io::stdin().is_terminal()
    {
        eprintln!("[berimor] конфигурация не найдена — первый запуск.");
        eprint!("[berimor] настроить провайдеры моделей сейчас? [Y/n] ");
        use std::io::Write as _;
        let _ = std::io::stderr().flush();
        let mut answer = String::new();
        let _ = std::io::stdin().read_line(&mut answer);
        if !matches!(answer.trim().to_lowercase().as_str(), "n" | "no" | "нет") {
            if let Err(err) = setup::run_wizard() {
                eprintln!("[berimor] мастер настройки: {err}");
            }
        }
    }

    let resolved_config = match config::load(cli.config.as_deref()) {
        Ok(config) => config,
        Err(err) => {
            eprintln!("[berimor] {err}");
            return ExitCode::FAILURE;
        }
    };

    // Контракты из конфигурации (спека config-contracts, 2026-08-14):
    // регистрация в реестре E2 один раз, ДО диспетчеризации команд —
    // run/chat/observe/daemon/serve видят один и тот же состав.
    // Объявления уже проверены при загрузке (config::load), поэтому
    // повторная ошибка разбора здесь невозможна — expect осознанный.
    berimor_executors::structured_llm::set_config_contracts(
        resolved_config
            .contracts
            .iter()
            .map(|contract| {
                berimor_executors::structured_llm::ConfigContract::new(
                    contract.name.clone(),
                    contract.description.clone(),
                    contract
                        .schema
                        .as_deref()
                        .expect("schema_path разрешён в inline при загрузке"),
                )
                .expect("контракты конфигурации проверены при загрузке")
            })
            .collect(),
    );

    // Без подкоманды — chat (§20.13).
    match cli.command.unwrap_or(Command::Chat) {
        Command::Setup => {
            if let Err(err) = setup::run_wizard() {
                eprintln!("[berimor] {err}");
                return ExitCode::FAILURE;
            }
        }
        Command::Schedule { action } => {
            let result = match action {
                ScheduleAction::Add {
                    process,
                    every,
                    once_in,
                    input,
                } => daemon::schedule_add(&resolved_config, &process, &every, &once_in, &input),
                ScheduleAction::List => daemon::schedule_list(&resolved_config),
                ScheduleAction::Remove { id } => daemon::schedule_remove(&resolved_config, &id),
            };
            if let Err(err) = result {
                eprintln!("[berimor] {err}");
                return ExitCode::FAILURE;
            }
        }
        Command::Daemon { once, tick_cap } => {
            if let Err(err) = daemon::run_daemon(&resolved_config, once, tick_cap) {
                eprintln!("[berimor] {err}");
                return ExitCode::FAILURE;
            }
        }
        Command::Sessions => {
            if let Err(err) = sessions::cmd_sessions(&resolved_config) {
                eprintln!("[berimor] {err}");
                return ExitCode::FAILURE;
            }
        }
        Command::Serve { port } => {
            if let Err(err) = serve::run(&resolved_config, port) {
                eprintln!("[berimor] {err}");
                return ExitCode::FAILURE;
            }
        }
        Command::McpServe => {
            return ExitCode::from(mcp_serve::serve() as u8);
        }
        Command::Memory { action } => match action {
            MemoryAction::Consolidate => {
                if let Err(err) = memory::consolidate(&resolved_config) {
                    eprintln!("[berimor] {err}");
                    return ExitCode::FAILURE;
                }
            }
        },
        Command::Chat => {
            // Chat грузит конфиг сам — и перегружает после /models add
            // (§20.12): resolved_config выше не используется.
            if let Err(err) = chat::cmd_chat(cli.config.as_deref().map(std::path::Path::new)) {
                eprintln!("[berimor] {err}");
                return ExitCode::FAILURE;
            }
        }
        Command::Run {
            process,
            resume,
            input,
            non_interactive,
        } => {
            // BR-05: флаг ИЛИ переменная окружения (демон/скрипты без
            // терминала, чтобы не править вызовы).
            let non_interactive =
                non_interactive || std::env::var_os("BERIMOR_NON_INTERACTIVE").is_some();
            if let Err(err) = run::run(&resolved_config, &process, &resume, &input, non_interactive)
            {
                eprintln!("[berimor] {err}");
                // Находка 3.16 аудита: отказ человека на human_gate —
                // отличимый код 2, не «сбой» (1): скрипты различают
                // «остановлено по решению оператора» и «упало».
                if matches!(err, run::RunError::HumanDeclined) {
                    return ExitCode::from(2);
                }
                return ExitCode::FAILURE;
            }
        }
        Command::Verify { artifact } => {
            let artifact_path = PathBuf::from(&artifact);
            match verify::verify_artifact(&artifact_path) {
                Ok(()) => println!("[berimor] подпись подтверждена: `{artifact}`"),
                Err(err) => {
                    eprintln!("[berimor] подпись НЕ подтверждена: `{artifact}` — {err}");
                    return ExitCode::FAILURE;
                }
            }
        }
        Command::SelfUpdate { resume } => {
            if let Err(err) = self_update::run(&resolved_config, &resume) {
                eprintln!("[berimor] {err}");
                return ExitCode::FAILURE;
            }
        }
        Command::Plugin { action } => match action {
            PluginAction::InstallLocal {
                source,
                allow_unsigned,
            } => {
                if let Err(err) = plugin_install::install_local(&source, allow_unsigned) {
                    eprintln!("[berimor] локальная установка плагина: {err}");
                    std::process::exit(1);
                }
            }
            PluginAction::Install {
                repo,
                resume,
                signer_workflow,
                allowed_ref,
                capability_ceiling,
            } => {
                if let Err(err) = plugin_install::run(
                    &resolved_config,
                    &repo,
                    &resume,
                    signer_workflow.as_deref(),
                    allowed_ref.as_deref(),
                    capability_ceiling.as_deref(),
                ) {
                    eprintln!("[berimor] {err}");
                    return ExitCode::FAILURE;
                }
            }
            PluginAction::Remove { name } => match plugin_install::remove(&name) {
                Ok(path) => println!("удалено: {}", path.display()),
                Err(err) => {
                    eprintln!("[berimor] {err}");
                    return ExitCode::FAILURE;
                }
            },
        },
        Command::Skill { action } => {
            let code = ext_cmd::run(ext_cmd::ExtKind::Skill, action);
            if code != 0 {
                std::process::exit(code);
            }
        }
        Command::Agent { action } => {
            let code = ext_cmd::run(ext_cmd::ExtKind::Agent, action);
            if code != 0 {
                std::process::exit(code);
            }
        }
        Command::Trust { action } => {
            let result = match action {
                TrustAction::Add {
                    repo,
                    allowed_ref,
                    signer_workflow,
                    capability_ceiling,
                } => {
                    let ceiling: Vec<String> = capability_ceiling
                        .split(',')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string)
                        .collect();
                    trust::add(
                        &resolved_config,
                        &repo,
                        &allowed_ref,
                        &signer_workflow,
                        &ceiling,
                    )
                }
                TrustAction::Remove { repo } => trust::remove(&resolved_config, &repo),
                TrustAction::List => trust::list(&resolved_config),
            };
            if let Err(err) = result {
                eprintln!("[berimor] {err}");
                return ExitCode::FAILURE;
            }
        }
        Command::Config { action } => match action {
            ConfigAction::Show => {
                println!("{resolved_config:#?}");
            }
        },
        Command::Login {
            provider,
            manual,
            list,
        } => {
            if list {
                match oauth::list() {
                    Ok(profiles) if profiles.is_empty() => {
                        eprintln!("[berimor] oauth-профилей нет — `berimor login --provider claude|openai`");
                    }
                    Ok(profiles) => {
                        for status in profiles {
                            let state = if status.expired {
                                "access истёк (обновится прозрачно)"
                            } else {
                                "access действителен"
                            };
                            let refresh = if status.has_refresh {
                                ", refresh есть"
                            } else {
                                ""
                            };
                            println!(
                                "{}\t{} до {}{}\t{}",
                                status.provider,
                                state,
                                status.expires_at_unix,
                                refresh,
                                status.token_url
                            );
                        }
                    }
                    Err(err) => {
                        eprintln!("[berimor] {err}");
                        return ExitCode::FAILURE;
                    }
                }
            } else {
                let Some(provider) = provider else {
                    eprintln!("[berimor] укажите --provider claude|openai (или --list)");
                    return ExitCode::FAILURE;
                };
                if let Err(err) = oauth::login(&provider, manual) {
                    eprintln!("[berimor] {err}");
                    return ExitCode::FAILURE;
                }
            }
        }
        Command::Logout { provider } => {
            if let Err(err) = oauth::logout(&provider) {
                eprintln!("[berimor] {err}");
                return ExitCode::FAILURE;
            }
        }
        Command::Trace { instance } => {
            if let Err(err) = observe::trace(&resolved_config, &instance) {
                eprintln!("[berimor] {err}");
                return ExitCode::FAILURE;
            }
        }
        Command::Cost { instance } => {
            if let Err(err) = observe::cost(&resolved_config, &instance) {
                eprintln!("[berimor] {err}");
                return ExitCode::FAILURE;
            }
        }
        Command::Eval { golden_dir } => {
            if let Err(err) = observe::eval(&resolved_config, &golden_dir) {
                eprintln!("[berimor] {err}");
                return ExitCode::FAILURE;
            }
        }
    }

    ExitCode::SUCCESS
}
