//! Определения субагентов (§20.17): `agent.yaml` из
//! `~/.config/berimor/agents/` (глобальные) и `.berimor/agents/`
//! (проектные — сильнее при совпадении имён). Формат — контракт
//! репозитория berimor-agents. Возможности ребёнка — ПОДМНОЖЕСТВО
//! прав родителя: пересечение вычисляет код, запросить больше нельзя.

use std::path::Path;

#[derive(Debug, Clone)]
pub struct AgentDef {
    pub name: String,
    pub description: String,
    pub model_tier: Option<String>,
    pub tools: Vec<String>,
    pub max_turns: u32,
    pub max_wall_seconds: u64,
    /// Право порождать субагентов самому (контракт berimor-agents;
    /// по умолчанию — запрещено, fail-closed).
    pub allow_spawn: bool,
    /// Тело prompt.md — системный контекст субагента (если есть рядом).
    pub prompt: String,
}

impl Default for AgentDef {
    fn default() -> Self {
        Self {
            name: String::new(),
            description: String::new(),
            model_tier: None,
            tools: Vec::new(),
            max_turns: 12,
            max_wall_seconds: 300,
            allow_spawn: false,
            prompt: String::new(),
        }
    }
}

/// Парсит agent.yaml — плоский YAML-подмножество (как skills, без
/// serde_yaml): скаляры, список tools, вложенный budget.
pub fn parse(text: &str) -> Result<AgentDef, String> {
    let mut def = AgentDef::default();
    let mut section: Option<&'static str> = None;
    for line in text.lines() {
        let trimmed = line.trim_end();
        if trimmed.trim_start().starts_with('#') || trimmed.is_empty() {
            continue;
        }
        if let Some(item) = trimmed.trim_start().strip_prefix("- ") {
            if section == Some("tools") {
                def.tools.push(item.trim().trim_matches('"').to_string());
            }
            continue;
        }
        let Some((key, value)) = trimmed.split_once(':') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim().trim_matches('"');
        let nested = line.starts_with(' ') || line.starts_with('\t');
        if nested {
            if section == Some("budget") {
                match key {
                    "max_turns" => {
                        def.max_turns = value.parse().unwrap_or(def.max_turns);
                    }
                    "max_wall_seconds" => {
                        def.max_wall_seconds = value.parse().unwrap_or(def.max_wall_seconds);
                    }
                    _ => {}
                }
            }
            continue;
        }
        section = None;
        match key {
            "name" => def.name = value.to_string(),
            "description" => def.description = value.to_string(),
            "model_tier" => def.model_tier = Some(value.to_string()),
            "tools" => section = Some("tools"),
            "budget" => section = Some("budget"),
            "allow_spawn" => def.allow_spawn = value == "true",
            _ => {}
        }
    }
    if def.name.is_empty() {
        return Err("agent.yaml: пустое name".into());
    }
    Ok(def)
}

fn load_dir(root: &Path, out: &mut Vec<AgentDef>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.filter_map(|e| e.ok()) {
        let dir = entry.path();
        let manifest = dir.join("agent.yaml");
        if !manifest.is_file() {
            continue;
        }
        match std::fs::read_to_string(&manifest)
            .map_err(|e| e.to_string())
            .and_then(|text| parse(&text))
        {
            Ok(mut def) => {
                // prompt.md рядом — системный контекст (опционально).
                if let Ok(prompt) = std::fs::read_to_string(dir.join("prompt.md")) {
                    def.prompt = prompt.trim().to_string();
                }
                out.push(def);
            }
            Err(err) => eprintln!("· субагент {} пропущен: {err}", dir.display()),
        }
    }
}

/// Все определения: глобальные, затем проектные поверх (по имени).
pub fn load_all(workspace: &Path) -> Vec<AgentDef> {
    let mut defs: Vec<AgentDef> = Vec::new();
    if let Some(global) = crate::config::global_dir() {
        load_dir(&global.join("agents"), &mut defs);
    }
    let mut project: Vec<AgentDef> = Vec::new();
    load_dir(&workspace.join(".berimor/agents"), &mut project);
    for def in project {
        defs.retain(|d| d.name != def.name);
        defs.push(def);
    }
    defs.sort_by(|a, b| a.name.cmp(&b.name));
    defs
}

/// Потолок ребёнка: agent.tools ∩ права родителя (встроенные имена).
/// Код, не модель; пустой tools — потолка нет (наследует права родителя).
pub fn ceiling(def: &AgentDef, parent_tools: &[String]) -> Option<Vec<String>> {
    if def.tools.is_empty() {
        return None;
    }
    Some(
        def.tools
            .iter()
            .filter(|t| parent_tools.contains(t))
            .cloned()
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "name: researcher
version: 0.1.0
description: Исследователь
model_tier: strong
tools:
  - files.read
  - http.fetch
budget:
  max_turns: 20
  max_wall_seconds: 300
jail: inherit
returns: summary
allow_spawn: true
";

    #[test]
    fn parses_agent_yaml() {
        let def = parse(SAMPLE).unwrap();
        assert_eq!(def.name, "researcher");
        assert_eq!(def.tools, vec!["files.read", "http.fetch"]);
        assert_eq!(def.max_turns, 20);
        assert_eq!(def.max_wall_seconds, 300);
        assert!(def.allow_spawn);
    }

    #[test]
    fn ceiling_is_intersection_not_extension() {
        let def = parse(SAMPLE).unwrap();
        let parent = vec!["files.read".to_string(), "files.write".to_string()];
        let c = ceiling(&def, &parent).unwrap();
        assert_eq!(c, vec!["files.read"]); // http.fetch вне прав родителя — отрезан
        assert!(!c.contains(&"files.write".to_string())); // ребёнок не расширяется
    }
}
