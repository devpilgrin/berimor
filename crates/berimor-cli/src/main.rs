//! `berimor` — точка входа: CLI, загрузка конфигурации, оркестрация подкоманд.
//!
//! Источник: `docs/arch/stack.md` §2, `docs/arch/deployment.md` §5. Подкоманда `verify`
//! существует потому, что bootstrap-слой (npm, TypeScript) не дублирует
//! криптографическую проверку на другом языке — вызывает эту же подкоманду
//! (ADR-0025). ROADMAP: F3.

use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::process::ExitCode;

mod config;
mod mcp_dispatch;
mod observe;
mod run;

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

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
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
    SelfUpdate,
    /// Установить плагин из доверенного репозитория.
    Plugin {
        #[command(subcommand)]
        action: PluginAction,
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
    Install { repo: String },
}

#[derive(Subcommand)]
enum ConfigAction {
    /// Напечатать конфигурацию, которая реально будет использована.
    Show,
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    let resolved_config = match config::load(cli.config.as_deref()) {
        Ok(config) => config,
        Err(err) => {
            eprintln!("[berimor] {err}");
            return ExitCode::FAILURE;
        }
    };

    match cli.command {
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
            eprintln!("todo(ROADMAP D2): проверить подпись `{artifact}`");
        }
        Command::SelfUpdate => {
            eprintln!(
                "todo(ROADMAP D4): agent-self-update (канал: {:?})",
                resolved_config.update_channel
            );
        }
        Command::Plugin { action } => match action {
            PluginAction::Install { repo } => {
                eprintln!("todo(ROADMAP D6): установить плагин из `{repo}`");
            }
        },
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
