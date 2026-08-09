//! Команды `berimor skill|agent list|install|remove` (§20.16) — работа
//! с каталогами расширений. Установка — копирование из git-каталога в
//! глобальную директорию (или проектную, `--project`). Операторские
//! команды: агентному диспетчеру недоступны, гейт не требуется.

use clap::Subcommand;
use std::path::PathBuf;

#[derive(Subcommand)]
pub enum ExtAction {
    /// Установленные; `--available` — доступные в каталоге.
    List {
        /// Показать содержимое каталога (обновляет кэш).
        #[arg(long)]
        available: bool,
    },
    /// Установить из каталога по имени.
    Install {
        name: String,
        /// В проект (`.berimor/...`), а не глобально.
        #[arg(long)]
        project: bool,
        /// Свой URL каталога вместо официального.
        #[arg(long)]
        repo: Option<String>,
        /// Установить из произвольного git-репозитория (§20.19):
        /// ищется `<repo>/<subdir>/<маркер>`, `<repo>/skills|agents/<name>/`
        /// или маркер в корне (репозиторий и есть расширение).
        #[arg(long)]
        from: Option<String>,
        /// Подкаталог внутри --from с манифестом (по умолчанию — автопоиск).
        #[arg(long)]
        path: Option<String>,
        /// Ветка/тег для --from (по умолчанию — HEAD).
        #[arg(long = "ref")]
        git_ref: Option<String>,
    },
    /// Удалить установленное.
    Remove {
        name: String,
        /// Из проекта (`.berimor/...`).
        #[arg(long)]
        project: bool,
    },
}

/// Тип расширения — параметризует каталог, префикс и маркер манифеста.
#[derive(Clone, Copy)]
pub enum ExtKind {
    Skill,
    Agent,
}

impl ExtKind {
    pub(crate) fn default_repo(&self) -> &'static str {
        match self {
            ExtKind::Skill => crate::catalog::DEFAULT_SKILLS_REPO,
            ExtKind::Agent => crate::catalog::DEFAULT_AGENTS_REPO,
        }
    }

    pub(crate) fn prefix(&self) -> &'static str {
        match self {
            ExtKind::Skill => "skills",
            ExtKind::Agent => "agents",
        }
    }

    pub(crate) fn marker(&self) -> &'static str {
        match self {
            ExtKind::Skill => "SKILL.md",
            ExtKind::Agent => "agent.yaml",
        }
    }

    fn label(&self) -> &'static str {
        match self {
            ExtKind::Skill => "скиллов",
            ExtKind::Agent => "субагентов",
        }
    }
}

pub(crate) fn dest_root(kind: &ExtKind, project: bool) -> Result<PathBuf, String> {
    if project {
        let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
        Ok(cwd.join(".berimor").join(kind.prefix()))
    } else {
        crate::config::global_dir()
            .map(|dir| dir.join(kind.prefix()))
            .ok_or_else(|| "глобальная директория недоступна".to_string())
    }
}

pub fn run(kind: ExtKind, action: ExtAction) -> i32 {
    match action {
        ExtAction::List { available } => list(&kind, available),
        ExtAction::Install {
            name,
            project,
            repo,
            from,
            path,
            git_ref,
        } => {
            if let Some(url) = &from {
                install_from_git(
                    &kind,
                    &name,
                    url,
                    path.as_deref(),
                    git_ref.as_deref(),
                    project,
                )
            } else {
                install(&kind, &name, project, repo.as_deref())
            }
        }
        ExtAction::Remove { name, project } => remove(&kind, &name, project),
    }
}

fn list(kind: &ExtKind, available: bool) -> i32 {
    if available {
        let repo = kind.default_repo();
        match crate::catalog::sync(repo) {
            Ok(dir) => {
                let entries = crate::catalog::list(&dir, kind.prefix(), kind.marker());
                if entries.is_empty() {
                    println!("каталог пуст ({})", kind.prefix());
                } else {
                    println!("доступно в каталоге ({repo}):");
                    for entry in entries {
                        println!("  {:24} {}", entry.name, entry.summary);
                    }
                }
                0
            }
            Err(err) => {
                eprintln!("каталог недоступен: {err}");
                1
            }
        }
    } else {
        let global = dest_root(kind, false).unwrap_or_default();
        let project = dest_root(kind, true).unwrap_or_default();
        let mut shown = 0usize;
        for (root, scope) in [(&global, "глобально"), (&project, "проект")] {
            let Ok(entries) = std::fs::read_dir(root) else {
                continue;
            };
            for entry in entries.filter_map(|e| e.ok()) {
                if entry.path().is_dir() && entry.path().join(kind.marker()).is_file() {
                    println!(
                        "  {:24} {:10} {}",
                        entry.file_name().to_string_lossy(),
                        scope,
                        entry.path().display()
                    );
                    shown += 1;
                }
            }
        }
        if shown == 0 {
            println!(
                "ничего не установлено. Каталог: berimor {} list --available; \
                 установка: berimor {} install <имя>",
                kind_label_cmd(kind),
                kind_label_cmd(kind)
            );
        }
        0
    }
}

fn kind_label_cmd(kind: &ExtKind) -> &'static str {
    match kind {
        ExtKind::Skill => "skill",
        ExtKind::Agent => "agent",
    }
}

/// Установка из произвольного git-репозитория (§20.19): клон →
/// автопоиск манифеста (`--path` → `<repo>/<prefix>s/<name>/` → корень)
/// → проверка маркера → атомарное размещение.
fn install_from_git(
    kind: &ExtKind,
    name: &str,
    url: &str,
    subdir: Option<&str>,
    git_ref: Option<&str>,
    project: bool,
) -> i32 {
    let root = match dest_root(kind, project) {
        Ok(root) => root,
        Err(err) => {
            eprintln!("{err}");
            return 1;
        }
    };
    let clone = match crate::catalog::git_clone(url, git_ref) {
        Ok(clone) => clone,
        Err(err) => {
            eprintln!("клонирование не удалось: {err}");
            return 1;
        }
    };
    let result = (|| {
        // Кандидаты в порядке приоритета.
        let mut candidates: Vec<std::path::PathBuf> = Vec::new();
        if let Some(subdir) = subdir {
            candidates.push(clone.join(subdir));
        }
        candidates.push(clone.join(kind.prefix()).join(name));
        candidates.push(clone.clone());
        let marker = kind.marker();
        let source = candidates
            .iter()
            .find(|dir| dir.join(marker).is_file())
            .ok_or_else(|| {
                format!(
                    "манифест {marker} не найден: ни в --path, ни в {}/{name}/, ни в корне репозитория",
                    kind.prefix()
                )
            })?;
        crate::catalog::place(source, name, &root)
    })();
    let _ = std::fs::remove_dir_all(&clone);
    match result {
        Ok(path) => {
            println!("установлено из {url}: {}", path.display());
            0
        }
        Err(err) => {
            eprintln!("установка не удалась: {err}");
            1
        }
    }
}

fn install(kind: &ExtKind, name: &str, project: bool, repo: Option<&str>) -> i32 {
    let repo = repo.unwrap_or_else(|| kind.default_repo());
    let root = match dest_root(kind, project) {
        Ok(root) => root,
        Err(err) => {
            eprintln!("{err}");
            return 1;
        }
    };
    match crate::catalog::install(repo, kind.prefix(), name, &root) {
        Ok(path) => {
            println!("установлено: {}", path.display());
            0
        }
        Err(err) => {
            eprintln!("установка не удалась: {err}");
            eprintln!(
                "· список каталога {}: berimor {} list --available",
                kind.label(),
                kind_label_cmd(kind)
            );
            1
        }
    }
}

fn remove(kind: &ExtKind, name: &str, project: bool) -> i32 {
    match remove_installed(kind, name, project) {
        Ok(target) => {
            println!("удалено: {}", target.display());
            0
        }
        Err(err) => {
            eprintln!("{err}");
            1
        }
    }
}

/// Чистая логика удаления — без println!/exit-кодов, переиспользуется
/// TUI (`chat_tui.rs::run_command("... remove")`), не только CLI.
pub(crate) fn remove_installed(
    kind: &ExtKind,
    name: &str,
    project: bool,
) -> Result<PathBuf, String> {
    let root =
        dest_root(kind, project).map_err(|_| "глобальная директория недоступна".to_string())?;
    let target = root.join(name);
    if !target.is_dir() {
        return Err(format!("'{name}' не установлен ({})", root.display()));
    }
    std::fs::remove_dir_all(&target).map_err(|err| format!("удаление не удалось: {err}"))?;
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Отрицательный путь — безопасен для параллельных `cargo test`: не
    /// трогает реальную глобальную директорию (`dest_root(project:
    /// false)` — платформенный `~/.config/berimor`), только читает.
    /// Позитивный путь (реальное удаление) — CWD-зависим
    /// (`dest_root(project: true)` = `cwd.join(".berimor")`), проверен
    /// живым прогоном в TUI, не юнит-тестом (та же осторожность, что
    /// уберегла от инцидента со случайным note.txt в исходниках пакета
    /// в этой же сессии ранее).
    #[test]
    fn remove_installed_errors_when_not_installed() {
        let result = remove_installed(
            &ExtKind::Skill,
            "definitely-not-a-real-skill-anywhere-xyz123",
            false,
        );
        assert!(result.is_err());
    }
}
