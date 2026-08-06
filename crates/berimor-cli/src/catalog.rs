//! Каталоги расширений berimor (§20.16): репозитории скилов, плагинов и
//! субагентов. «Видеть и устанавливать» — через git: каталог клонируется
//! в кэш глобальной директории и обновляется `git pull` — детерминированно,
//! без API-токенов и лимитов GitHub REST.
//!
//! Каталог по умолчанию — три официальных репозитория; оператор может
//! указать свои URL (корпоративный каталог — тот же механизм).

use std::path::PathBuf;

/// Официальные каталоги (тип содержимого → репозиторий). Плагины идут
/// отдельным конвейером D6 (подписанные релизы, trust list) — каталог
/// для них не нужен.
pub const DEFAULT_SKILLS_REPO: &str = "https://github.com/devpilgrin/berimor-skills";
pub const DEFAULT_AGENTS_REPO: &str = "https://github.com/devpilgrin/berimor-agents";

/// Элемент каталога: имя и краткое описание (из манифеста).
#[derive(Debug, Clone)]
pub struct CatalogEntry {
    pub name: String,
    pub summary: String,
}

fn cache_root() -> Option<PathBuf> {
    crate::config::global_dir().map(|dir| dir.join("catalog-cache"))
}

/// Путь локального кэша репозитория каталога (по имени репозитория).
fn cache_dir(repo_url: &str) -> Option<PathBuf> {
    let name = repo_url.trim_end_matches('/').rsplit('/').next()?;
    Some(cache_root()?.join(name))
}

/// Клонирует git-репозиторий во временный каталог (depth 1; `--ref` —
/// ветка/тег). Для установки из произвольных репозиториев (§20.19).
/// Возвращает путь к клону (временный каталог удаляет вызывающий).
pub(crate) fn git_clone(url: &str, git_ref: Option<&str>) -> Result<std::path::PathBuf, String> {
    let dest = std::env::temp_dir().join(format!(
        "berimor-clone-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0)
    ));
    let mut command = std::process::Command::new("git");
    command.arg("clone").arg("--depth").arg("1").arg("--quiet");
    if let Some(r) = git_ref {
        command.arg("--branch").arg(r);
    }
    command.arg(url).arg(&dest);
    let output = command
        .output()
        .map_err(|e| format!("git недоступен: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "git clone {url}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(dest)
}

/// Клонирует (или обновляет) кэш каталога. Возвращает корень кэша.
/// git — внешняя команда ОПЕРАТОРСКОГО уровня (не агента): этот модуль
/// вызывается только CLI-командами, агентному диспетчеру недоступен.
pub fn sync(repo_url: &str) -> Result<PathBuf, String> {
    let dir = cache_dir(repo_url).ok_or("некорректный URL каталога")?;
    if dir.join(".git").is_dir() {
        let status = std::process::Command::new("git")
            .args(["-C", &dir.to_string_lossy(), "pull", "--ff-only", "-q"])
            .status()
            .map_err(|e| format!("git pull: {e}"))?;
        if !status.success() {
            return Err(format!("git pull в {} завершился ошибкой", dir.display()));
        }
        return Ok(dir);
    }
    if let Some(parent) = dir.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("создание кэша: {e}"))?;
    }
    let status = std::process::Command::new("git")
        .args([
            "clone",
            "--depth",
            "1",
            "-q",
            repo_url,
            &dir.to_string_lossy(),
        ])
        .status()
        .map_err(|e| format!("git clone: {e}"))?;
    if !status.success() {
        return Err(format!("git clone {repo_url} завершился ошибкой"));
    }
    Ok(dir)
}

/// Список элементов каталога: поддиректории `<dir>/<prefix>/<name>` с
/// манифестом (`marker`), имя и первая строка description из манифеста.
pub fn list(dir: &std::path::Path, prefix: &str, marker: &str) -> Vec<CatalogEntry> {
    let root = dir.join(prefix);
    let Ok(entries) = std::fs::read_dir(&root) else {
        return Vec::new();
    };
    let mut out: Vec<CatalogEntry> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir() && e.path().join(marker).is_file())
        .map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            let summary = std::fs::read_to_string(e.path().join(marker))
                .ok()
                .and_then(|text| {
                    text.lines()
                        .find(|l| l.trim_start().starts_with("description:"))
                        .map(|l| {
                            l.trim_start()
                                .trim_start_matches("description:")
                                .trim()
                                .to_string()
                        })
                })
                .unwrap_or_default();
            CatalogEntry { name, summary }
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Копирует элемент каталога в целевую директорию (атомарно через
/// переименование: сначала во временную, затем на место).
pub fn install(
    repo_url: &str,
    prefix: &str,
    name: &str,
    dest_root: &std::path::Path,
) -> Result<PathBuf, String> {
    // Имя — строго [a-z0-9-]: попадает в файловый путь (та же дисциплина,
    // что validate_plugin_name в D6).
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return Err(format!("имя '{name}' недопустимо: ожидается [a-z0-9-]+"));
    }
    let dir = sync(repo_url)?;
    let source = dir.join(prefix).join(name);
    if !source.is_dir() {
        return Err(format!("'{name}' не найден в каталоге ({prefix}/)"));
    }
    place(&source, name, dest_root)
}

/// Размещает каталог-источник в dest_root под именем (атомарно через
/// staging + rename). Общая часть установки из каталога и из
/// произвольного git-репозитория (§20.19).
pub(crate) fn place(
    source: &std::path::Path,
    name: &str,
    dest_root: &std::path::Path,
) -> Result<PathBuf, String> {
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return Err(format!("имя '{name}' недопустимо: ожидается [a-z0-9-]+"));
    }
    let target = dest_root.join(name);
    let staging = dest_root.join(format!(".staging-{name}"));
    std::fs::create_dir_all(dest_root)
        .map_err(|e| format!("создание {}: {e}", dest_root.display()))?;
    if staging.exists() {
        let _ = std::fs::remove_dir_all(&staging);
    }
    copy_dir(source, &staging).map_err(|e| format!("копирование: {e}"))?;
    if target.exists() {
        std::fs::remove_dir_all(&target)
            .map_err(|e| format!("замена {}: {e}", target.display()))?;
    }
    std::fs::rename(&staging, &target).map_err(|e| format!("установка: {e}"))?;
    Ok(target)
}

fn copy_dir(from: &std::path::Path, to: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let dest = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &dest)?;
        } else {
            std::fs::copy(entry.path(), &dest)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Локальный git-репозиторий как каталог — без сети.
    fn local_catalog() -> (PathBuf, PathBuf) {
        let base = std::env::temp_dir().join(format!("berimor-cat-{}", std::process::id()));
        let repo = base.join("repo");
        std::fs::create_dir_all(repo.join("skills/demo-skill")).unwrap();
        std::fs::write(
            repo.join("skills/demo-skill/SKILL.md"),
            "---\nname: demo-skill\ndescription: Демонстрационный скилл\n---\n\n# тело\n",
        )
        .unwrap();
        std::fs::create_dir_all(repo.join("agents/demo-agent")).unwrap();
        std::fs::write(
            repo.join("agents/demo-agent/agent.yaml"),
            "name: demo-agent\ndescription: Демонстрационный субагент\n",
        )
        .unwrap();
        for args in [
            vec!["init", "-q"],
            vec!["add", "-A"],
            vec![
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=t",
                "commit",
                "-qm",
                "init",
            ],
        ] {
            let status = std::process::Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(&args)
                .status()
                .unwrap();
            assert!(status.success(), "git {:?} failed", args);
        }
        (base, repo)
    }

    #[test]
    fn list_and_install_from_local_git_catalog() {
        let (base, repo) = local_catalog();
        let url = repo.to_string_lossy().to_string();
        // Кэш в изолированной temp-директории — не трогаем глобальную.
        let cache = base.join("cache");
        let dir = cache.join(repo.file_name().unwrap());
        let status = std::process::Command::new("git")
            .args(["clone", "--depth", "1", "-q", &url])
            .arg(&dir)
            .status()
            .unwrap();
        assert!(status.success());

        let entries = list(&dir, "skills", "SKILL.md");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "demo-skill");
        assert_eq!(entries[0].summary, "Демонстрационный скилл");

        // install через sync() использует глобальный кэш — проверяем
        // копирующую часть напрямую, без глобального состояния.
        let dest = base.join("installed");
        std::fs::create_dir_all(&dest).unwrap();
        let staging = dest.join(".staging-demo-skill");
        copy_dir(&dir.join("skills/demo-skill"), &staging).unwrap();
        std::fs::rename(&staging, dest.join("demo-skill")).unwrap();
        assert!(dest.join("demo-skill/SKILL.md").is_file());

        // Валидация имени.
        assert!(install(&url, "skills", "../escape", &dest).is_err());
        std::fs::remove_dir_all(&base).ok();
    }
}
