//! Линт скиллов и субагентов (0.32.0; идея манифеста и готовности —
//! razzant/ouroboros: skill_manifest/skill_readiness, перенесена на наш
//! формат). Проверяет то, что можно проверить статически и ДЕШЁВО, до
//! установки и до коммита в каталог. Fail-closed: ошибка линта —
//! отказ установки; предупреждение — печать, не отказ.
//!
//! Семантика `permissions` (декларация намерения, не гейт): каждое
//! разрешение объясняет, зачем скиллу его инструменты. Инструмент без
//! объявленного разрешения — ошибка (скилл просит больше, чем признаёт);
//! разрешение без инструмента — предупреждение (мёртвая декларация).
//! Исполнение ограничено потолком `tools` независимо от деклараций.

use std::path::Path;

use crate::builtin_dispatch::BUILTIN_TOOLS;
use crate::skills::Skill;

/// Декларируемые разрешения скилла (frontmatter `permissions:`).
pub const KNOWN_PERMISSIONS: &[&str] = &["net", "exec", "fs-write", "spawn"];

/// Разрешение → инструменты, которые оно объясняет.
fn permission_tools(permission: &str) -> &'static [&'static str] {
    match permission {
        "net" => &["http.fetch", "web.search"],
        "exec" => &[
            "terminal.exec",
            "terminal.start",
            "terminal.output",
            "terminal.kill",
        ],
        "fs-write" => &["files.write", "files.edit"],
        "spawn" => &["agents.run"],
        _ => &[],
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Level {
    Error,
    Warning,
}

#[derive(Debug)]
pub struct LintIssue {
    pub level: Level,
    pub message: String,
}

impl LintIssue {
    fn error(message: impl Into<String>) -> Self {
        Self {
            level: Level::Error,
            message: message.into(),
        }
    }
    fn warning(message: impl Into<String>) -> Self {
        Self {
            level: Level::Warning,
            message: message.into(),
        }
    }
}

/// Статические проверки скилла. Чистая функция — тестируется без ФС.
pub fn lint_skill(skill: &Skill) -> Vec<LintIssue> {
    let mut issues = Vec::new();
    // Имя: контракт каталога (как в репозитории berimor-skills).
    let name_ok = !skill.name.is_empty()
        && skill.name.len() <= 64
        && skill
            .name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_');
    if !name_ok {
        issues.push(LintIssue::error(format!(
            "name '{}' не соответствует контракту каталога (a-z0-9-_, ≤64)",
            skill.name
        )));
    }
    if skill.description.trim().is_empty() {
        issues.push(LintIssue::error("пустое description"));
    }
    if skill.version.trim().is_empty() {
        issues.push(LintIssue::warning("нет version — каталогу нужна версия"));
    }
    if skill.body.trim().is_empty() {
        issues.push(LintIssue::error("пустое тело SKILL.md"));
    }
    // Инструменты: известные встроенные — ок; имена с точкой-сервером,
    // которых нет в реестре, — возможный MCP (предупреждение, статически
    // не проверить); явный мусор — ошибка.
    for tool in &skill.tools {
        if BUILTIN_TOOLS.contains(&tool.as_str()) {
            continue;
        }
        if tool
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
            && tool.contains('.')
        {
            issues.push(LintIssue::warning(format!(
                "инструмент '{tool}' не из встроенного реестра — MCP-сервер? статически не проверяется"
            )));
        } else {
            issues.push(LintIssue::error(format!(
                "инструмент '{tool}' неизвестен и не похож на имя MCP (сервер.инструмент)"
            )));
        }
    }
    // Разрешения: известность + согласованность с потолком инструментов.
    for permission in &skill.permissions {
        if !KNOWN_PERMISSIONS.contains(&permission.as_str()) {
            issues.push(LintIssue::error(format!(
                "неизвестное разрешение '{permission}' (известные: {})",
                KNOWN_PERMISSIONS.join(", ")
            )));
        }
    }
    for tool in &skill.tools {
        for permission in KNOWN_PERMISSIONS {
            if permission_tools(permission).contains(&tool.as_str())
                && !skill.permissions.iter().any(|p| p == permission)
            {
                issues.push(LintIssue::error(format!(
                    "инструмент '{tool}' требует объявления permissions: {permission}"
                )));
            }
        }
    }
    for permission in &skill.permissions {
        let tools = permission_tools(permission);
        if !tools.is_empty() && !skill.tools.iter().any(|t| tools.contains(&t.as_str())) {
            issues.push(LintIssue::warning(format!(
                "разрешение '{permission}' объявлено, но ни одного его инструмента в tools нет"
            )));
        }
    }
    issues
}

/// Линт по пути (каталог скилла с SKILL.md или сам файл). Печатает
/// находки, возвращает true, если ошибок нет.
pub fn lint_path(path: &Path) -> Result<bool, String> {
    let file = if path.is_dir() {
        path.join("SKILL.md")
    } else {
        path.to_path_buf()
    };
    let text = std::fs::read_to_string(&file)
        .map_err(|err| format!("не удалось прочитать {}: {err}", file.display()))?;
    let skill = crate::skills::parse(&text, path)?;
    let issues = lint_skill(&skill);
    let mut has_errors = false;
    for issue in &issues {
        let mark = match issue.level {
            Level::Error => {
                has_errors = true;
                "ошибка"
            }
            Level::Warning => "предупреждение",
        };
        println!("{mark}: {}: {}", skill.name, issue.message);
    }
    if issues.is_empty() {
        println!("скилл '{}' — линт чист", skill.name);
    }
    Ok(!has_errors)
}

/// Линт субагента (agent.yaml + prompt.md рядом). Те же принципы:
/// известные инструменты, контракт имени, непустой prompt.
pub fn lint_agent_path(path: &Path) -> Result<bool, String> {
    let dir = if path.is_dir() {
        path.to_path_buf()
    } else {
        path.parent()
            .ok_or_else(|| format!("нет родительского каталога у {}", path.display()))?
            .to_path_buf()
    };
    let file = dir.join("agent.yaml");
    let text = std::fs::read_to_string(&file)
        .map_err(|err| format!("не удалось прочитать {}: {err}", file.display()))?;
    let def = crate::agents::parse(&text)?;
    let prompt = std::fs::read_to_string(dir.join("prompt.md")).unwrap_or_default();
    let mut issues: Vec<LintIssue> = Vec::new();
    let name_ok = !def.name.is_empty()
        && def.name.len() <= 64
        && def
            .name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_');
    if !name_ok {
        issues.push(LintIssue::error(format!(
            "name '{}' не соответствует контракту каталога",
            def.name
        )));
    }
    if def.description.trim().is_empty() {
        issues.push(LintIssue::error("пустое description"));
    }
    if prompt.trim().is_empty() {
        issues.push(LintIssue::error("пустой или отсутствующий prompt.md"));
    }
    for tool in &def.tools {
        if BUILTIN_TOOLS.contains(&tool.as_str()) {
            continue;
        }
        if tool
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
            && tool.contains('.')
        {
            issues.push(LintIssue::warning(format!(
                "инструмент '{tool}' не из встроенного реестра — MCP-сервер?"
            )));
        } else {
            issues.push(LintIssue::error(format!("инструмент '{tool}' неизвестен")));
        }
    }
    match def.model_tier.as_deref() {
        Some("weak" | "strong") | None => {}
        Some(other) => issues.push(LintIssue::error(format!(
            "model_tier '{other}' — допустимы weak|strong"
        ))),
    }
    let mut has_errors = false;
    for issue in &issues {
        let mark = match issue.level {
            Level::Error => {
                has_errors = true;
                "ошибка"
            }
            Level::Warning => "предупреждение",
        };
        println!("{mark}: {}: {}", def.name, issue.message);
    }
    if issues.is_empty() {
        println!("субагент '{}' — линт чист", def.name);
    }
    Ok(!has_errors)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn skill(tools: &[&str], permissions: &[&str]) -> Skill {
        Skill {
            name: "demo-skill".into(),
            version: "1.0.0".into(),
            description: "демо".into(),
            tools: tools.iter().map(|s| s.to_string()).collect(),
            permissions: permissions.iter().map(|s| s.to_string()).collect(),
            body: "тело".into(),
            ..Skill::default()
        }
    }

    fn errors(issues: &[LintIssue]) -> Vec<&str> {
        issues
            .iter()
            .filter(|i| i.level == Level::Error)
            .map(|i| i.message.as_str())
            .collect()
    }

    #[test]
    fn read_only_skill_is_clean() {
        let issues = lint_skill(&skill(&["files.read", "files.search"], &[]));
        assert!(errors(&issues).is_empty(), "{issues:?}");
    }

    #[test]
    fn undeclared_permission_for_tool_is_error() {
        let issues = lint_skill(&skill(&["terminal.exec"], &[]));
        let errs = errors(&issues);
        assert!(errs.iter().any(|m| m.contains("exec")), "{errs:?}");
    }

    #[test]
    fn declared_permission_covers_tool() {
        let issues = lint_skill(&skill(&["terminal.exec"], &["exec"]));
        assert!(errors(&issues).is_empty(), "{issues:?}");
    }

    #[test]
    fn unknown_permission_is_error() {
        let issues = lint_skill(&skill(&["files.read"], &["teleport"]));
        assert!(errors(&issues).iter().any(|m| m.contains("teleport")));
    }

    #[test]
    fn unknown_tool_name_is_error_mcp_shape_is_warning() {
        let issues = lint_skill(&skill(&["totally bogus"], &[]));
        assert!(!errors(&issues).is_empty());
        let issues = lint_skill(&skill(&["github.create_issue"], &[]));
        assert!(errors(&issues).is_empty(), "MCP-форма — предупреждение");
        assert!(issues.iter().any(|i| i.level == Level::Warning));
    }

    #[test]
    fn empty_body_is_error() {
        let mut skill = skill(&["files.read"], &[]);
        skill.body = String::new();
        assert!(!errors(&lint_skill(&skill)).is_empty());
    }

    #[test]
    fn permission_without_tool_is_warning() {
        let issues = lint_skill(&skill(&["files.read"], &["net"]));
        assert!(errors(&issues).is_empty());
        assert!(issues
            .iter()
            .any(|i| i.level == Level::Warning && i.message.contains("net")));
    }
}
