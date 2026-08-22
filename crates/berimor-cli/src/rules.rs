//! Слой правил (0.37.0; перенос идеи Harness AI Rules на нашу
//! архитектуру): естественно-языковые стандарты в markdown-файлах,
//! подмешиваемые в контекст модели ДО генерации — мягкое формирование
//! вывода. Жёсткие гейты остаются за медиацией и capability-слоем
//! (их формула «rules guide early, policy enforces hard» — наша
//! медиация и есть hard-слой).
//!
//! Источники (оба опциональны, отсутствие — не ошибка):
//! - глобальные: `<config-dir>/rules/*.md` (~/.config/berimor/rules/);
//! - проектные: `.berimor/rules/*.md` (сильнее — идут ПОЗЖЕ в контексте,
//!   их приоритет scope'ов: узкий перекрывает широкий).
//!
//! Файлы внутри каталога — по имени (сортировка), каждый — отдельным
//! блоком с заголовком.

use std::path::{Path, PathBuf};

use berimor_context_engine::{ContextBuilder, ContextLayer};
use berimor_types::model::ModelTier;

/// Блоки правил: глобальные (в порядке имён), затем проектные.
pub fn load_rules(config_dir: &Path, workspace: &Path) -> Vec<String> {
    let mut blocks = Vec::new();
    collect_md(&config_dir.join("rules"), "глобальное", &mut blocks);
    collect_md(
        &workspace.join(".berimor").join("rules"),
        "проектное",
        &mut blocks,
    );
    blocks
}

fn collect_md(dir: &Path, scope: &str, out: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|ext| ext == "md"))
        .collect();
    files.sort();
    for file in files {
        if let Ok(content) = std::fs::read_to_string(&file) {
            let content = content.trim();
            if !content.is_empty() {
                let name = file.file_name().unwrap_or_default().to_string_lossy();
                out.push(format!("Правило ({scope}, {name}):\n{content}"));
            }
        }
    }
}

/// Построитель-декоратор: слои внутреннего построителя + блоки правил
/// сразу ПОСЛЕ системного правила (ранняя позиция — раньше состояния
/// задачи и памяти; их «before users spend time editing output»).
pub struct RulesContextBuilder<'a> {
    pub inner: &'a dyn ContextBuilder,
    pub rules: Vec<String>,
}

impl ContextBuilder for RulesContextBuilder<'_> {
    fn build(
        &self,
        step_kind: &str,
        tier: ModelTier,
        state: &serde_json::Value,
        task_hint: &str,
    ) -> Vec<ContextLayer> {
        let mut layers = self.inner.build(step_kind, tier, state, task_hint);
        if self.rules.is_empty() {
            return layers;
        }
        let rule_layers: Vec<ContextLayer> = self
            .rules
            .iter()
            .map(|content| ContextLayer {
                name: "project_rule".into(),
                content: content.clone(),
                weight: 0.9,
            })
            .collect();
        // Позиция 1 — сразу после system_rules (если он есть).
        let pos = usize::from(!layers.is_empty());
        layers.splice(pos..pos, rule_layers);
        layers
    }
}

/// Обвязка: inner-построитель + правила (глобальные из каталога конфига,
/// проектные из `.berimor/rules` текущей директории).
pub fn wrap<'a>(
    inner: &'a dyn ContextBuilder,
    config: &crate::config::Config,
) -> RulesContextBuilder<'a> {
    let config_dir = config
        .storage_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();
    let workspace = std::env::current_dir().unwrap_or_default();
    RulesContextBuilder {
        inner,
        rules: load_rules(&config_dir, &workspace),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use berimor_context_engine::SimpleContextBuilder;

    #[test]
    fn rules_load_global_then_project_sorted() {
        let root = std::env::temp_dir().join(format!("berimor-rules-{}", std::process::id()));
        let global = root.join("cfg/rules");
        let project = root.join("ws/.berimor/rules");
        std::fs::create_dir_all(&global).unwrap();
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(global.join("b-second.md"), "Г2").unwrap();
        std::fs::write(global.join("a-first.md"), "Г1").unwrap();
        std::fs::write(project.join("z.md"), "П1").unwrap();
        std::fs::write(global.join("skip.txt"), "не md").unwrap();
        let blocks = load_rules(&root.join("cfg"), &root.join("ws"));
        assert_eq!(blocks.len(), 3);
        assert!(blocks[0].contains("Г1") && blocks[0].contains("глобальное"));
        assert!(blocks[1].contains("Г2"));
        assert!(blocks[2].contains("П1") && blocks[2].contains("проектное"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn missing_dirs_are_not_an_error() {
        let blocks = load_rules(Path::new("/nonexistent-cfg"), Path::new("/nonexistent-ws"));
        assert!(blocks.is_empty());
    }

    #[test]
    fn builder_appends_rules_after_system_layer() {
        let inner = SimpleContextBuilder;
        let builder = RulesContextBuilder {
            inner: &inner,
            rules: vec!["Правило (проектное, x.md):\nвсе коммиты на русском".into()],
        };
        let layers = builder.build(
            "llm_structured",
            ModelTier::Strong,
            &serde_json::json!({}),
            "s1",
        );
        assert_eq!(layers[0].name, "system_rules");
        assert_eq!(layers[1].name, "project_rule");
        assert!(layers[1].content.contains("на русском"));
        // Без правил — делегирование без изменений.
        let plain = RulesContextBuilder {
            inner: &inner,
            rules: vec![],
        };
        let layers = plain.build(
            "llm_structured",
            ModelTier::Strong,
            &serde_json::json!({}),
            "s1",
        );
        assert!(layers.iter().all(|l| l.name != "project_rule"));
    }
}
