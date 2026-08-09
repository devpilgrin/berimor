//! Рантайм плагинов (§20.18): вызов инструментов установленных плагинов
//! как процессов. Контракт (репозиторий berimor-plugins):
//!
//! - плагин — исполняемый файл `installed/<name>/<name>`;
//! - вызов: `<binary> <tool>` с JSON-аргументами на stdin;
//! - ответ — JSON на stdout: `{"content": ...}` или `{"error": "..."}`;
//! - таймаут 30 с (как terminal.exec), вывод — 64 КиБ;
//! - регистрируются ТОЛЬКО инструменты из манифеста; mutates — из
//!   манифеста в политику гейта (не из догадок кода).

use berimor_capability::plugin::{PluginManifest, PluginToolDecl};
use berimor_executors::tool_only::{DispatchError, ToolDispatch};
use serde_json::Value;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(30);
const MAX_OUTPUT: usize = 64 * 1024;

/// Один установленный плагин: манифест + путь к бинарнику.
struct InstalledPlugin {
    manifest: PluginManifest,
    binary: PathBuf,
}

/// Диспетчер инструментов плагинов: поиск по декларациям манифестов,
/// вызов процессом с таймаутом. Диспетчер — не гейт: безопасность до
/// него (подпись/TOFU при установке, ACL-манифест, политика mutates).
pub(crate) struct PluginRuntimeDispatch {
    plugins: Vec<InstalledPlugin>,
}

impl PluginRuntimeDispatch {
    /// Сканирует `plugins_root/installed/*`: манифест валиден + бинарник
    /// на месте. Пусто/битые — пропуск с заметкой, не отказ всего слоя.
    pub fn scan(plugins_root: &Path) -> Self {
        let mut plugins = Vec::new();
        let installed = plugins_root.join("installed");
        if let Ok(entries) = std::fs::read_dir(&installed) {
            for entry in entries.filter_map(|e| e.ok()) {
                let dir = entry.path();
                let manifest_path = dir.join("manifest.yaml");
                let Some(name) = dir.file_name().and_then(|n| n.to_str()) else {
                    continue;
                };
                let binary = dir.join(name);
                if !manifest_path.is_file() || !binary.is_file() {
                    continue;
                }
                match berimor_capability::plugin::load_manifest(&manifest_path) {
                    Ok(manifest) => plugins.push(InstalledPlugin { manifest, binary }),
                    Err(err) => eprintln!("· плагин {name} пропущен: {err}"),
                }
            }
        }
        Self { plugins }
    }

    pub fn is_empty(&self) -> bool {
        self.plugins.is_empty()
    }

    /// Имя + имена инструментов на плагин — для `berimor plugin list`/
    /// `/plugins` в TUI (§20.36). Манифест не несёт собственного
    /// description (только по-инструментно) — сводка из имён.
    pub fn summaries(&self) -> Vec<(String, Vec<String>)> {
        self.plugins
            .iter()
            .map(|p| {
                (
                    p.manifest.name.clone(),
                    p.manifest
                        .capabilities
                        .tools
                        .iter()
                        .map(|t| t.name.clone())
                        .collect(),
                )
            })
            .collect()
    }

    pub fn has_tool(&self, tool: &str) -> bool {
        self.plugins
            .iter()
            .any(|p| p.manifest.capabilities.tools.iter().any(|t| t.name == tool))
    }

    /// Политики гейта из манифестов: mutates декларируется плагином.
    pub fn policies(&self) -> Vec<(String, berimor_capability::confirm::ToolPolicy)> {
        let mut out = Vec::new();
        for plugin in &self.plugins {
            for tool in &plugin.manifest.capabilities.tools {
                out.push((
                    tool.name.clone(),
                    berimor_capability::confirm::ToolPolicy {
                        mutates: Some(tool.mutates),
                        ..Default::default()
                    },
                ));
            }
        }
        out
    }

    /// Декларации для каталога модели (имя + описание).
    pub fn tool_decls(&self) -> Vec<&PluginToolDecl> {
        self.plugins
            .iter()
            .flat_map(|p| p.manifest.capabilities.tools.iter())
            .collect()
    }
}

impl ToolDispatch for PluginRuntimeDispatch {
    fn call(&self, tool: &str, args: &Value) -> Result<Value, DispatchError> {
        let plugin = self
            .plugins
            .iter()
            .find(|p| p.manifest.capabilities.tools.iter().any(|t| t.name == tool))
            .ok_or_else(|| DispatchError {
                tool: tool.into(),
                reason: "инструмент плагина не найден (не декларирован в манифесте)".into(),
            })?;

        let mut child = Command::new(&plugin.binary)
            .arg(tool)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| DispatchError {
                tool: tool.into(),
                reason: format!("запуск плагина: {e}"),
            })?;
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(args.to_string().as_bytes());
        }

        // Таймаут — ожиданием в потоке (паттерн terminal.exec).
        let (tx, rx) = mpsc::channel();
        let handle = std::thread::spawn(move || {
            let result = child.wait_with_output();
            let _ = tx.send(result);
        });
        let output = match rx.recv_timeout(TIMEOUT) {
            Ok(Ok(output)) => output,
            Ok(Err(err)) => {
                let _ = handle.join();
                return Err(DispatchError {
                    tool: tool.into(),
                    reason: format!("плагин: {err}"),
                });
            }
            Err(_) => {
                return Err(DispatchError {
                    tool: tool.into(),
                    reason: format!("плагин не ответил за {} с", TIMEOUT.as_secs()),
                });
            }
        };
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stdout = if stdout.len() > MAX_OUTPUT {
            &stdout[..MAX_OUTPUT]
        } else {
            &stdout
        };
        if !output.status.success() {
            return Err(DispatchError {
                tool: tool.into(),
                reason: format!(
                    "плагин завершился с кодом {:?}: {}",
                    output.status.code(),
                    stdout.trim()
                ),
            });
        }
        let parsed: Value = serde_json::from_str(stdout.trim()).map_err(|e| DispatchError {
            tool: tool.into(),
            reason: format!("ответ плагина не JSON ({{\"content\"|\"error\"}}): {e}"),
        })?;
        if let Some(error) = parsed.get("error").and_then(Value::as_str) {
            return Err(DispatchError {
                tool: tool.into(),
                reason: format!("плагин: {error}"),
            });
        }
        Ok(parsed.get("content").cloned().unwrap_or(Value::Null))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plant_plugin(root: &Path) {
        let dir = root.join("installed/hello");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("manifest.yaml"),
            "name: hello\ncapabilities:\n  tools:\n    - name: hello.greet\n      description: Приветствие\n      mutates: false\n",
        )
        .unwrap();
        let script = dir.join("hello");
        std::fs::write(
            &script,
            "#!/bin/sh\nread -r args\necho \"{\\\"content\\\": \\\"Привет, $1 вызван\\\"}\"\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    #[test]
    #[cfg(unix)]
    fn plugin_tool_runs_as_process_and_returns_content() {
        let root = std::env::temp_dir().join(format!("berimor-plugin-rt-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        plant_plugin(&root);
        let dispatch = PluginRuntimeDispatch::scan(&root);
        assert!(dispatch.has_tool("hello.greet"));
        assert!(!dispatch.has_tool("hello.unknown"));
        let result = dispatch
            .call("hello.greet", &serde_json::json!({}))
            .unwrap();
        assert_eq!(result, serde_json::json!("Привет, hello.greet вызван"));
        let policies = dispatch.policies();
        assert_eq!(policies.len(), 1);
        assert_eq!(policies[0].1.mutates, Some(false));
    }

    /// §20.36: `/plugins` в TUI показывает установленные плагины и их
    /// инструменты — `summaries()` источник данных для этой команды.
    #[test]
    fn summaries_lists_installed_plugins_with_tool_names() {
        let root = std::env::temp_dir().join(format!("berimor-plugin-sum-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        plant_plugin(&root);
        let dispatch = PluginRuntimeDispatch::scan(&root);

        let summaries = dispatch.summaries();

        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].0, "hello");
        assert_eq!(summaries[0].1, vec!["hello.greet".to_string()]);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    #[cfg(unix)]
    fn undeclared_tool_rejected() {
        let root = std::env::temp_dir().join(format!("berimor-plugin-rt2-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        plant_plugin(&root);
        let dispatch = PluginRuntimeDispatch::scan(&root);
        assert!(dispatch
            .call("hello.unknown", &serde_json::json!({}))
            .is_err());
    }
}
