//! Отдача собственных инструментов по MCP (T2).
//!
//! Источник: `docs/arch/stack.md` §6, ADR-0023. ROADMAP: T2.
//!
//! Это МЕХАНИЗМ — сервер, отдающий по MCP произвольный набор
//! инструментов, заданный вызывающим кодом (`ToolDefinition`), а не
//! конкретные встроенные инструменты (терминал, файлы, VCS, ...) из
//! `ideal-agent-architecture.md` §3.9 — их реализация не входит в эту
//! задачу (список без реализации, как и в остальной части ROADMAP до
//! отдельных задач). Ресурсы и шаблоны из названия задачи T2 —
//! аналогично не реализованы: сама MCP-сессия/протокол-хендшейк и
//! маршрутизация `tools/*` — то немногое, что нужно исполнителям прямо
//! сейчас; `resources`/`prompts` — расширение по мере появления
//! конкретной задачи, не выдуманное здесь заранее.
//!
//! Регистрация инструментов — динамическая, через
//! `rmcp::handler::server::router::tool::ToolRoute::new_dyn`, не
//! макросы `#[tool_router]`/`#[tool]`: набор инструментов, которые
//! Berimor отдаёт, определяется во время выполнения вызывающим кодом
//! (например, обёрткой над `berimor_executors::tool_only::ToolDispatch`),
//! не фиксирован на этапе компиляции.

use rmcp::handler::server::router::tool::{ToolRoute, ToolRouter};
use rmcp::handler::server::tool::ToolCallContext;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, JsonObject, ListToolsResult,
    PaginatedRequestParams, ServerCapabilities, ServerInfo, Tool,
};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::ErrorData;
use rmcp::ServerHandler;
use serde_json::Value;
use std::sync::Arc;

/// Один инструмент, отдаваемый по MCP: имя, описание, JSON Schema
/// аргументов и обработчик. Обработчик синхронный по сигнатуре (сам
/// может быть async внутри через блокирующий вызов — здесь важен только
/// контракт входа/выхода) и получает уже распарсенные аргументы как
/// JSON-объект — тот же контракт, что у
/// `berimor_executors::tool_only::ToolDispatch::call`, чтобы существующие
/// реализации можно было обернуть без переписывания их внутренней логики.
#[derive(Clone)]
pub struct ToolDefinition {
    name: String,
    description: String,
    input_schema: JsonObject,
    #[allow(clippy::type_complexity)]
    handler: Arc<dyn Fn(JsonObject) -> Result<Value, String> + Send + Sync>,
}

impl ToolDefinition {
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        input_schema: JsonObject,
        handler: impl Fn(JsonObject) -> Result<Value, String> + Send + Sync + 'static,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            input_schema,
            handler: Arc::new(handler),
        }
    }
}

/// MCP-сервер, отдающий заданный на этапе конструирования набор
/// инструментов. `Clone` дешёвый — `ToolRouter` внутри хранит маршруты
/// за `Arc`.
#[derive(Clone)]
pub struct McpServer {
    router: ToolRouter<Self>,
    instructions: Option<String>,
}

impl McpServer {
    pub fn new(
        tools: impl IntoIterator<Item = ToolDefinition>,
        instructions: Option<String>,
    ) -> Self {
        let mut router = ToolRouter::new();
        for definition in tools {
            let handler = definition.handler.clone();
            let tool = Tool::new(
                definition.name,
                definition.description,
                Arc::new(definition.input_schema),
            );
            router.add_route(ToolRoute::new_dyn(
                tool,
                move |ctx: ToolCallContext<Self>| {
                    let handler = handler.clone();
                    let arguments = ctx.arguments.clone().unwrap_or_default();
                    Box::pin(async move {
                        let result = match handler(arguments) {
                            Ok(value) => CallToolResult::structured(value),
                            Err(reason) => CallToolResult::structured_error(
                                serde_json::json!({ "error": reason }),
                            ),
                        };
                        Ok(result.into())
                    })
                },
            ));
        }
        Self {
            router,
            instructions,
        }
    }
}

impl ServerHandler for McpServer {
    fn get_info(&self) -> ServerInfo {
        let capabilities = ServerCapabilities::builder().enable_tools().build();
        match &self.instructions {
            Some(instructions) => {
                ServerInfo::new(capabilities).with_instructions(instructions.clone())
            }
            None => ServerInfo::new(capabilities),
        }
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        self.router
            .call(ToolCallContext::new(self, request, context))
            .await
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult {
            tools: self.router.list_all(),
            ..Default::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp_client::McpClient;
    use serde_json::json;

    fn echo_tool() -> ToolDefinition {
        ToolDefinition::new(
            "echo",
            "Возвращает аргумент как есть",
            JsonObject::new(),
            |arguments| Ok(Value::Object(arguments)),
        )
    }

    fn failing_tool() -> ToolDefinition {
        ToolDefinition::new(
            "fail",
            "Всегда падает",
            JsonObject::new(),
            |_arguments| Err("намеренный сбой теста".to_string()),
        )
    }

    async fn connected_pair(server: McpServer) -> (McpClient, tokio::task::JoinHandle<()>) {
        let (server_half, client_half) = tokio::io::duplex(8192);
        let server_task = tokio::spawn(async move {
            let running = rmcp::ServiceExt::serve(server, server_half).await.unwrap();
            running.waiting().await.unwrap();
        });
        let client = McpClient::connect(client_half).await.unwrap();
        (client, server_task)
    }

    #[tokio::test]
    async fn list_tools_reports_every_registered_definition() {
        let server = McpServer::new([echo_tool(), failing_tool()], None);
        let (client, server_task) = connected_pair(server).await;

        let tools = client.list_tools().await.unwrap();

        assert_eq!(tools.len(), 2);
        assert!(tools.iter().any(|t| t.name == "echo"));
        assert!(tools.iter().any(|t| t.name == "fail"));

        client.close().await.unwrap();
        server_task.abort();
    }

    #[tokio::test]
    async fn call_tool_invokes_the_handler_and_returns_structured_content() {
        let server = McpServer::new([echo_tool()], None);
        let (client, server_task) = connected_pair(server).await;

        let mut arguments = JsonObject::new();
        arguments.insert("text".into(), json!("hello"));
        let result = client.call_tool("echo", Some(arguments)).await.unwrap();

        assert_eq!(result.is_error, Some(false));
        assert_eq!(result.structured_content, Some(json!({"text": "hello"})));

        client.close().await.unwrap();
        server_task.abort();
    }

    #[tokio::test]
    async fn call_tool_handler_error_becomes_a_structured_error_result() {
        let server = McpServer::new([failing_tool()], None);
        let (client, server_task) = connected_pair(server).await;

        let result = client.call_tool("fail", None).await.unwrap();

        assert_eq!(result.is_error, Some(true));
        assert_eq!(
            result.structured_content,
            Some(json!({"error": "намеренный сбой теста"}))
        );

        client.close().await.unwrap();
        server_task.abort();
    }

    #[tokio::test]
    async fn call_tool_for_unregistered_name_is_a_protocol_error() {
        let server = McpServer::new([echo_tool()], None);
        let (client, server_task) = connected_pair(server).await;

        let result = client.call_tool("no-such-tool", None).await;

        assert!(result.is_err());

        client.close().await.unwrap();
        server_task.abort();
    }

    #[test]
    fn get_info_carries_the_given_instructions() {
        let server = McpServer::new([], Some("следуй инструкциям".to_string()));
        assert_eq!(
            server.get_info().instructions,
            Some("следуй инструкциям".to_string())
        );
    }

    #[test]
    fn get_info_without_instructions_leaves_it_none() {
        let server = McpServer::new([], None);
        assert_eq!(server.get_info().instructions, None);
    }
}
