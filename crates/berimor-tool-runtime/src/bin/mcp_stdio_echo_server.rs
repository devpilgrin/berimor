//! Тестовый helper-бинарник: минимальный MCP-сервер по stdio с одним
//! инструментом `echo`.
//!
//! Существует только для `McpClient::spawn` в тестах T1
//! (`mcp_client.rs`) — единственный способ проверить настоящий путь
//! «дочерний процесс + stdio», не через `tokio::io::duplex`, без внешней
//! зависимости от npm/python MCP-серверов в CI.

use rmcp::handler::server::router::tool::{ToolRoute, ToolRouter};
use rmcp::handler::server::tool::ToolCallContext;
use rmcp::model::{CallToolRequestParams, CallToolResponse, CallToolResult, ListToolsResult};
use rmcp::model::{PaginatedRequestParams, ServerCapabilities, ServerInfo, Tool};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::ServiceExt;
use rmcp::{ErrorData, ServerHandler};
use std::sync::Arc;

#[derive(Clone)]
struct EchoServer {
    router: ToolRouter<Self>,
}

impl EchoServer {
    fn new() -> Self {
        let mut router = ToolRouter::new();
        router.add_route(ToolRoute::new_dyn(
            Tool::new("echo", "Возвращает аргумент как есть", Arc::default()),
            |ctx: ToolCallContext<Self>| {
                let arguments = ctx.arguments.clone().unwrap_or_default();
                Box::pin(async move {
                    Ok(CallToolResult::structured(serde_json::Value::Object(arguments)).into())
                })
            },
        ));
        Self { router }
    }
}

impl ServerHandler for EchoServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
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

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let server = EchoServer::new()
        .serve(rmcp::transport::stdio())
        .await
        .expect("установить MCP-сессию по stdio");
    server.waiting().await.expect("дождаться завершения сессии");
}
