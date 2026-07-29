//! `berimor` — точка входа: CLI, загрузка конфигурации, оркестрация подкоманд.
//!
//! Источник: `arch/stack.md` §2, `arch/deployment.md` §5. Подкоманда `verify`
//! существует потому, что bootstrap-слой (npm, TypeScript) не дублирует
//! криптографическую проверку на другом языке — вызывает эту же подкоманду
//! (ADR-0025). ROADMAP: F3.

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "berimor",
    version,
    about = "Детерминированное ядро агентной системы"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Выполнить процесс (структурированную задачу).
    Run { process: String },
    /// Проверить подпись артефакта — вызывается bootstrap-слоем (ADR-0025).
    Verify { artifact: String },
    /// Прогнать процесс `agent-self-update` (ADR-0019).
    SelfUpdate,
    /// Установить плагин из доверенного репозитория.
    Plugin {
        #[command(subcommand)]
        action: PluginAction,
    },
}

#[derive(Subcommand)]
enum PluginAction {
    Install { repo: String },
}

fn main() {
    let cli = Cli::parse();
    match cli.command {
        Command::Run { process } => {
            eprintln!("todo(ROADMAP P3): выполнить процесс `{process}`");
        }
        Command::Verify { artifact } => {
            eprintln!("todo(ROADMAP D2): проверить подпись `{artifact}`");
        }
        Command::SelfUpdate => {
            eprintln!("todo(ROADMAP D4): agent-self-update");
        }
        Command::Plugin { action } => match action {
            PluginAction::Install { repo } => {
                eprintln!("todo(ROADMAP D6): установить плагин из `{repo}`");
            }
        },
    }
}
