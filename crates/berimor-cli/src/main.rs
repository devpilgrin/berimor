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
mod catalog;
mod chat;
mod chat_history;
mod chat_tui;
mod chat_ui;
mod config;
mod ext_cmd;
mod mcp_dispatch;
mod observe;
mod plugin_install;
mod presets;
mod run;
mod self_update;
mod setup;
mod skills;
mod trust;
mod verify;

#[derive(Parser)]
#[command(
    name = "berimor",
    version,
    about = "Детерминированное ядро агентной системы"
)]
struct Cli {
    /// Путь к файлу конфигурации. По умолчанию — `./berimor.toml`,
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
    /// Человекочитаемая трассировка журнала одного инстанса (O1).
    Trace {
        /// Идентификатор инстанса (тот же, что печатает `run`/`--resume`).
        instance: String,
    },
    /// Офлайн-прогон золотого набора: доля веток, доля отказов Mediation (O2).
    Eval {
        /// Директория с `process.yaml` и `<сценарий>.json` файлами входа.
        golden_dir: PathBuf,
    },
}

#[derive(Subcommand)]
enum PluginAction {
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

    // Без подкоманды — chat (§20.13).
    match cli.command.unwrap_or(Command::Chat) {
        Command::Setup => {
            if let Err(err) = setup::run_wizard() {
                eprintln!("[berimor] {err}");
                return ExitCode::FAILURE;
            }
        }
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
        } => {
            if let Err(err) = run::run(&resolved_config, &process, &resume, &input) {
                eprintln!("[berimor] {err}");
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
        Command::Trace { instance } => {
            if let Err(err) = observe::trace(&resolved_config, &instance) {
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
