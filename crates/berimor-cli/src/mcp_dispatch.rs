//! `tool`-шаги против настоящих MCP-серверов инструментов (T1), не только
//! статических заглушек `tool_stubs`.
//!
//! Источник: `docs/ROADMAP.md` §12 (T1, `berimor-tool-runtime::mcp_client`).
//! Только T1 — клиент к серверу, который оператор сам прописал в
//! `[[mcp_servers]]` конфига. T3 (`PluginProcess`, ACL-манифест
//! установленного плагина, `docs/ROADMAP.md` D6) сюда сознательно не
//! входит: установка плагина зависит от ещё не реализованных D4/D5
//! (`agent-self-update`, доверенный список) — соединять их здесь значило
//! бы придумывать процесс установки, которого ROADMAP пока не описывает.
//! Серверы из конфига доверены самим фактом присутствия в файле оператора
//! — как и сегодняшние `tool_stubs`.

use crate::config::McpServerConfig;
use berimor_executors::tool_only::{DispatchError, StaticToolDispatch, ToolDispatch};
use berimor_tool_runtime::mcp_client::{McpClient, McpClientError};
use rmcp::model::ContentBlock;
use serde_json::Value;
use std::collections::HashMap;

#[derive(Debug, thiserror::Error)]
pub enum McpDispatchError {
    #[error("не удалось подключиться к MCP-серверу '{name}': {source}")]
    Connect {
        name: String,
        #[source]
        source: McpClientError,
    },
    #[error("не удалось получить список инструментов сервера '{name}': {source}")]
    ListTools {
        name: String,
        #[source]
        source: McpClientError,
    },
    #[error(
        "инструмент '{tool}' объявлен и сервером '{first_server}', и сервером '{second_server}' — конфликт имён"
    )]
    DuplicateTool {
        tool: String,
        first_server: String,
        second_server: String,
    },
}

/// Один рантайм `tokio` на все MCP-клиенты диспетчера, создаётся один раз
/// при старте и живёт до конца прогона. `ToolDispatch::call` — синхронный
/// контракт (`tool_only::execute`), `McpClient` — асинхронный; `block_on`
/// в точке вызова — наименьший дифф для интеграции одного синхронного
/// шага в асинхронный клиент, без перевода всего `berimor run` на
/// `#[tokio::main]` ради этого.
pub struct McpToolDispatch {
    runtime: tokio::runtime::Runtime,
    clients: HashMap<String, McpClient>,
    tool_to_server: HashMap<String, String>,
}

impl McpToolDispatch {
    /// Подключается ко всем серверам из конфига и запоминает, какой
    /// сервер отдаёт какой инструмент (`tools/list` один раз при старте —
    /// не на каждый вызов). Конфликт имён между серверами — ошибка старта
    /// (I2: испорченную/противоречивую конфигурацию нельзя молча
    /// разрешать произвольным выбором одного из вариантов).
    pub fn connect(servers: &[McpServerConfig]) -> Result<Self, McpDispatchError> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("создать tokio-рантайм для MCP-клиентов инструментов");
        let mut clients = HashMap::new();
        let mut tool_to_server: HashMap<String, String> = HashMap::new();

        runtime.block_on(async {
            for server in servers {
                let mut command = tokio::process::Command::new(&server.command);
                command.args(&server.args);
                let client = McpClient::spawn(command).await.map_err(|source| {
                    McpDispatchError::Connect {
                        name: server.name.clone(),
                        source,
                    }
                })?;
                let tools =
                    client
                        .list_tools()
                        .await
                        .map_err(|source| McpDispatchError::ListTools {
                            name: server.name.clone(),
                            source,
                        })?;
                for tool in tools {
                    let tool_name = tool.name.to_string();
                    if let Some(first_server) =
                        tool_to_server.insert(tool_name.clone(), server.name.clone())
                    {
                        return Err(McpDispatchError::DuplicateTool {
                            tool: tool_name,
                            first_server,
                            second_server: server.name.clone(),
                        });
                    }
                }
                clients.insert(server.name.clone(), client);
            }
            Ok(())
        })?;

        Ok(Self {
            runtime,
            clients,
            tool_to_server,
        })
    }

    pub fn has_tool(&self, tool: &str) -> bool {
        self.tool_to_server.contains_key(tool)
    }
}

impl Drop for McpToolDispatch {
    /// Без этого закрытие сессий полагалось бы на закрытие файловых
    /// дескрипторов ОС при завершении процесса `berimor` и на то,
    /// заметит ли внешний сервер EOF в stdin сам — случайная
    /// корректность, не гарантия (найдено независимым ревью интеграции
    /// CLI-M1/M2/M3). `McpClient::close()` асинхронный и требует `self`
    /// по значению — `mem::take` вынимает клиентов из карты, чтобы
    /// закрыть их через собственный рантайм до его собственного `Drop`.
    fn drop(&mut self) {
        let clients = std::mem::take(&mut self.clients);
        self.runtime.block_on(async {
            for (_, client) in clients {
                let _ = client.close().await;
            }
        });
    }
}

impl ToolDispatch for McpToolDispatch {
    fn call(&self, tool: &str, args: &Value) -> Result<Value, DispatchError> {
        let server_name = self.tool_to_server.get(tool).ok_or_else(|| DispatchError {
            tool: tool.to_string(),
            reason: "инструмент не зарегистрирован ни на одном MCP-сервере".into(),
        })?;
        let client = self
            .clients
            .get(server_name)
            .expect("сервер зарегистрирован в tool_to_server вместе с клиентом");

        let arguments = match args {
            Value::Object(map) => Some(map.clone()),
            Value::Null => None,
            other => {
                let mut wrapper = serde_json::Map::new();
                wrapper.insert("value".into(), other.clone());
                Some(wrapper)
            }
        };

        let result = self
            .runtime
            .block_on(client.call_tool(tool, arguments))
            .map_err(|err| DispatchError {
                tool: tool.to_string(),
                reason: err.to_string(),
            })?;

        if result.is_error == Some(true) {
            return Err(DispatchError {
                tool: tool.to_string(),
                reason: content_to_text(&result.content),
            });
        }
        Ok(result
            .structured_content
            .unwrap_or_else(|| Value::String(content_to_text(&result.content))))
    }
}

fn content_to_text(content: &[ContentBlock]) -> String {
    content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text(text) => Some(text.text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Порядок разрешения инструмента (§20.10): встроенные (зарезервированное
/// пространство имён — ни MCP, ни заглушка не перекрывают их) →
/// зарегистрированные MCP-серверы → статические заглушки `tool_stubs`.
/// Единственная реализация `ToolDispatch` в `run.rs` — `tool_only::execute`
/// не меняется вовсе.
pub struct CompositeToolDispatch {
    pub builtin: crate::builtin_dispatch::BuiltinToolDispatch,
    pub mcp: Option<McpToolDispatch>,
    pub static_stubs: StaticToolDispatch,
}

impl ToolDispatch for CompositeToolDispatch {
    fn call(&self, tool: &str, args: &Value) -> Result<Value, DispatchError> {
        if crate::builtin_dispatch::BuiltinToolDispatch::has_tool(tool) {
            return self.builtin.call(tool, args);
        }
        if let Some(mcp) = &self.mcp {
            if mcp.has_tool(tool) {
                return mcp.call(tool, args);
            }
        }
        self.static_stubs.call(tool, args)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn composite_dispatch_falls_back_to_static_stubs_without_mcp() {
        let dispatch = CompositeToolDispatch {
            builtin: crate::builtin_dispatch::BuiltinToolDispatch::new(std::path::PathBuf::from(
                ".",
            )),
            mcp: None,
            static_stubs: StaticToolDispatch::new(vec![(
                "echo".into(),
                serde_json::json!({"ok": true}),
                false,
            )]),
        };

        let result = dispatch.call("echo", &Value::Null).unwrap();
        assert_eq!(result, serde_json::json!({"ok": true}));
    }

    #[test]
    fn composite_dispatch_errors_for_unknown_tool_same_as_static_dispatch_alone() {
        let dispatch = CompositeToolDispatch {
            builtin: crate::builtin_dispatch::BuiltinToolDispatch::new(std::path::PathBuf::from(
                ".",
            )),
            mcp: None,
            static_stubs: StaticToolDispatch::new(Vec::new()),
        };

        assert!(dispatch.call("no-such-tool", &Value::Null).is_err());
    }
}
