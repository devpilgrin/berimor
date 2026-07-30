//! Изоляция плагина как процесса + ACL (T3).
//!
//! Источник: `ideal-agent-architecture.md` §3.9. ROADMAP: T3.
//!
//! IPC-транспорт из названия задачи — то, что уже даёт T1
//! (`McpClient::spawn`, дочерний процесс по stdio): не переизобретается
//! здесь заново. Этот модуль добавляет то, чего T1 сознательно не
//! делал — применение ACL-манифеста плагина (S6,
//! `berimor_capability::plugin::PluginManifest`) к вызовам конкретного
//! дочернего процесса. «Источник ACL — статический манифест на диске,
//! который сам компонент переопределить не может» (`security-model.md`
//! §4): манифест передаётся вызывающим кодом (хостом) при создании
//! `PluginProcess`, сам процесс плагина его не видит и не может
//! изменить — то же разделение, что у `PluginRegistry` в S6.
//!
//! `capability_ceiling` манифеста — «имена инструментов или классов
//! действий» — здесь читается буквально как allow-list имён MCP-
//! инструментов, которые хосту разрешено вызывать у этого плагина: даже
//! если процесс плагина технически ответит на вызов, хост откажет раньше,
//! чем сообщение вообще уйдёт наружу, если имени нет в потолке.

use crate::mcp_client::{McpClient, McpClientError};
use berimor_capability::plugin::{PluginAclError, PluginManifest};
use rmcp::model::{CallToolResult, JsonObject, Tool};
use rmcp::service::RoleClient;
use rmcp::transport::IntoTransport;

#[derive(Debug, thiserror::Error)]
pub enum PluginProcessError {
    #[error(transparent)]
    Acl(#[from] PluginAclError),
    #[error(transparent)]
    Client(#[from] McpClientError),
}

/// Плагин, запущенный как изолированный процесс: MCP-сессия к его stdio
/// (T1) плюс манифест ACL (S6), против которого проверяется каждый вызов.
pub struct PluginProcess {
    client: McpClient,
    manifest: PluginManifest,
}

impl PluginProcess {
    /// Устанавливает сессию поверх произвольного транспорта — как и у
    /// `McpClient::connect`, используется и `spawn` (дочерний процесс),
    /// и тестами (`tokio::io::duplex`) без дублирования логики.
    pub async fn connect<T, E, A>(
        transport: T,
        manifest: PluginManifest,
    ) -> Result<Self, PluginProcessError>
    where
        T: IntoTransport<RoleClient, E, A>,
        E: std::error::Error + Send + Sync + 'static,
    {
        let client = McpClient::connect(transport).await?;
        Ok(Self { client, manifest })
    }

    /// Запускает `command` как дочерний процесс плагина и проверяет его
    /// последующие вызовы против `manifest`.
    pub async fn spawn(
        command: tokio::process::Command,
        manifest: PluginManifest,
    ) -> Result<Self, PluginProcessError> {
        let client = McpClient::spawn(command).await?;
        Ok(Self { client, manifest })
    }

    /// Инструменты плагина, отфильтрованные его потолком capability —
    /// вызывающий код видит только то, что реально сможет вызвать, не
    /// полный список, который отдаёт процесс.
    pub async fn list_tools(&self) -> Result<Vec<Tool>, PluginProcessError> {
        let tools = self.client.list_tools().await?;
        Ok(tools
            .into_iter()
            .filter(|tool| self.manifest.check_capability(&tool.name).is_ok())
            .collect())
    }

    /// Вызывает инструмент плагина ПОСЛЕ проверки ACL — отказ манифеста
    /// останавливает вызов раньше, чем что-либо уходит процессу.
    pub async fn call_tool(
        &self,
        name: impl AsRef<str> + Into<String>,
        arguments: Option<JsonObject>,
    ) -> Result<CallToolResult, PluginProcessError> {
        self.manifest.check_capability(name.as_ref())?;
        let result = self.client.call_tool(name.into(), arguments).await?;
        Ok(result)
    }

    pub async fn close(self) -> Result<(), PluginProcessError> {
        self.client.close().await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp_server::{McpServer, ToolDefinition};
    use serde_json::{json, Value};

    fn manifest(capability_ceiling: &[&str]) -> PluginManifest {
        PluginManifest {
            name: "test-plugin".into(),
            allowed_events: vec![],
            allowed_secrets: vec![],
            capability_ceiling: capability_ceiling.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn server_with_two_tools() -> McpServer {
        McpServer::new(
            [
                ToolDefinition::new(
                    "allowed_tool",
                    "Разрешённый инструмент",
                    JsonObject::new(),
                    |args| Ok(Value::Object(args)),
                ),
                ToolDefinition::new(
                    "forbidden_tool",
                    "Запрещённый инструмент",
                    JsonObject::new(),
                    |_| Ok(json!("не должно быть вызвано")),
                ),
            ],
            None,
        )
    }

    async fn connected_plugin(
        manifest: PluginManifest,
    ) -> (PluginProcess, tokio::task::JoinHandle<()>) {
        let (server_half, client_half) = tokio::io::duplex(8192);
        let server_task = tokio::spawn(async move {
            let running = rmcp::ServiceExt::serve(server_with_two_tools(), server_half)
                .await
                .unwrap();
            running.waiting().await.unwrap();
        });
        let plugin = PluginProcess::connect(client_half, manifest).await.unwrap();
        (plugin, server_task)
    }

    #[tokio::test]
    async fn list_tools_hides_tools_outside_the_capability_ceiling() {
        let (plugin, server_task) = connected_plugin(manifest(&["allowed_tool"])).await;

        let tools = plugin.list_tools().await.unwrap();

        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "allowed_tool");

        plugin.close().await.unwrap();
        server_task.abort();
    }

    #[tokio::test]
    async fn call_tool_within_ceiling_reaches_the_plugin_process() {
        let (plugin, server_task) = connected_plugin(manifest(&["allowed_tool"])).await;

        let mut arguments = JsonObject::new();
        arguments.insert("x".into(), json!(1));
        let result = plugin
            .call_tool("allowed_tool", Some(arguments))
            .await
            .unwrap();

        assert_eq!(result.structured_content, Some(json!({"x": 1})));

        plugin.close().await.unwrap();
        server_task.abort();
    }

    #[tokio::test]
    async fn call_tool_outside_ceiling_is_rejected_before_reaching_the_process() {
        let (plugin, server_task) = connected_plugin(manifest(&["allowed_tool"])).await;

        let result = plugin.call_tool("forbidden_tool", None).await;

        assert!(matches!(
            result,
            Err(PluginProcessError::Acl(
                PluginAclError::CapabilityNotAllowed { .. }
            ))
        ));

        plugin.close().await.unwrap();
        server_task.abort();
    }

    #[tokio::test]
    async fn empty_ceiling_denies_every_tool_fail_closed() {
        let (plugin, server_task) = connected_plugin(manifest(&[])).await;

        let tools = plugin.list_tools().await.unwrap();
        assert!(tools.is_empty());

        let result = plugin.call_tool("allowed_tool", None).await;
        assert!(result.is_err());

        plugin.close().await.unwrap();
        server_task.abort();
    }
}
