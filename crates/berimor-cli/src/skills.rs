//! Загрузчик скилов (§20.16): пакеты поведения из
//! `~/.config/berimor/skills/` (глобальные) и `.berimor/skills/`
//! (проектные — сильнее при совпадении имён). Формат — контракт
//! репозитория berimor-skills: `SKILL.md` с YAML-подобным frontmatter
//! (плоский: строки, списки с `- `) и телом — системным контекстом.
//!
//! Принципы:
//! - триггер срабатывает КОДОМ (точное совпадение slash-команды или
//!   префикс фразы), модель выбор скилла не делает;
//! - `tools` скилла — потолок: применяется как пересечение с правами
//!   сессии (FilteringToolDispatch в chat), не расширение;
//! - парсер минимальный и строгий: формат наш, serde_yaml не нужен.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default)]
pub struct Skill {
    pub name: String,
    pub version: String,
    pub description: String,
    pub triggers: Vec<String>,
    pub tools: Vec<String>,
    pub model_tier: Option<String>,
    /// Тело SKILL.md — системный контекст модели.
    pub body: String,
    /// Откуда загружен (для диагностики /skills).
    pub origin: PathBuf,
}

/// Парсит SKILL.md: frontmatter между первыми двумя строками `---`.
pub fn parse(text: &str, origin: &Path) -> Result<Skill, String> {
    let mut lines = text.lines();
    if lines.next().map(str::trim) != Some("---") {
        return Err("нет frontmatter (первая строка — не ---)".into());
    }
    let mut skill = Skill {
        origin: origin.to_path_buf(),
        ..Skill::default()
    };
    let mut current_list: Option<&'static str> = None;
    let mut body_start = None;
    for (index, line) in text.lines().enumerate().skip(1) {
        let trimmed = line.trim_end();
        if trimmed == "---" {
            body_start = Some(index + 1);
            break;
        }
        if let Some(item) = trimmed.trim_start().strip_prefix("- ") {
            let item = item.trim().trim_matches('"').to_string();
            match current_list {
                Some("triggers") => skill.triggers.push(item),
                Some("tools") => skill.tools.push(item),
                _ => return Err(format!("элемент списка вне ключа: {trimmed}")),
            }
            continue;
        }
        let Some((key, value)) = trimmed.split_once(':') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim().trim_matches('"');
        current_list = None;
        match key {
            "name" => skill.name = value.to_string(),
            "version" => skill.version = value.to_string(),
            "description" => skill.description = value.to_string(),
            "model_tier" => skill.model_tier = Some(value.to_string()),
            "triggers" => current_list = Some("triggers"),
            "tools" => current_list = Some("tools"),
            _ => {} // неизвестные ключи — вперёд-совместимо
        }
    }
    let Some(body_start) = body_start else {
        return Err("frontmatter не закрыт (вторая --- не найдена)".into());
    };
    skill.body = text
        .lines()
        .skip(body_start)
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string();
    if skill.name.is_empty() {
        return Err("frontmatter: пустое name".into());
    }
    Ok(skill)
}

fn load_dir(root: &Path, out: &mut Vec<Skill>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path().join("SKILL.md");
        if !path.is_file() {
            continue;
        }
        match std::fs::read_to_string(&path)
            .map_err(|e| e.to_string())
            .and_then(|text| parse(&text, &entry.path()))
        {
            Ok(skill) => out.push(skill),
            Err(err) => eprintln!("· скилл {} пропущен: {err}", entry.path().display()),
        }
    }
}

/// Все скиллы: глобальные, затем проектные поверх (по имени — проектные
/// сильнее). workspace — корень области (cwd).
pub fn load_all(workspace: &Path) -> Vec<Skill> {
    let mut skills: Vec<Skill> = Vec::new();
    if let Some(global) = crate::config::global_dir() {
        load_dir(&global.join("skills"), &mut skills);
    }
    let mut project: Vec<Skill> = Vec::new();
    load_dir(&workspace.join(".berimor/skills"), &mut project);
    for skill in project {
        skills.retain(|s| s.name != skill.name);
        skills.push(skill);
    }
    skills.sort_by(|a, b| a.name.cmp(&b.name));
    skills
}

/// Совпадение триггера КОДОМ: slash-команда (`/review` — первое слово)
/// или префикс фразы («проверь код…»). Встроенные slash-команды чата
/// обрабатываются раньше и триггеров не образуют.
pub fn match_trigger<'a>(skills: &'a [Skill], message: &str) -> Option<&'a Skill> {
    let lowered = message.trim().to_lowercase();
    skills.iter().find(|skill| {
        skill.triggers.iter().any(|trigger| {
            let trigger = trigger.trim().to_lowercase();
            if trigger.is_empty() {
                return false;
            }
            if let Some(cmd) = trigger.strip_prefix('/') {
                lowered
                    .split_whitespace()
                    .next()
                    .is_some_and(|first| first == cmd || first == trigger.as_str())
            } else {
                lowered.starts_with(&trigger)
            }
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "---
name: code-review-ru
version: 0.1.0
description: Ревью diff'ов
triggers:
  - \"проверь код\"
  - \"/review\"
tools:
  - files.read
  - terminal.exec
model_tier: strong
---

# Тело скилла

Инструкции модели.
";

    #[test]
    fn parses_frontmatter_and_body() {
        let skill = parse(SAMPLE, Path::new("/tmp/x")).unwrap();
        assert_eq!(skill.name, "code-review-ru");
        assert_eq!(skill.triggers, vec!["проверь код", "/review"]);
        assert_eq!(skill.tools, vec!["files.read", "terminal.exec"]);
        assert!(skill.body.contains("Инструкции модели."));
    }

    #[test]
    fn trigger_matching_is_code_exact() {
        let skill = parse(SAMPLE, Path::new("/tmp/x")).unwrap();
        let skills = vec![skill];
        assert!(match_trigger(&skills, "/review").is_some());
        assert!(match_trigger(&skills, "/review src/main.rs").is_some());
        assert!(match_trigger(&skills, "проверь код в src").is_some());
        assert!(match_trigger(&skills, "ПРОВЕРЬ КОД").is_some());
        assert!(match_trigger(&skills, "а проверь код").is_none()); // не префикс
        assert!(match_trigger(&skills, "ревьюшечка").is_none());
        assert!(match_trigger(&skills, "/help").is_none());
    }

    #[test]
    fn project_overrides_global_by_name() {
        let base = std::env::temp_dir().join(format!("berimor-skills-{}", std::process::id()));
        let project_skill = base.join(".berimor/skills/demo");
        std::fs::create_dir_all(&project_skill).unwrap();
        std::fs::write(project_skill.join("SKILL.md"), SAMPLE).unwrap();
        let skills = load_all(&base);
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "code-review-ru");
        std::fs::remove_dir_all(&base).ok();
    }
}
