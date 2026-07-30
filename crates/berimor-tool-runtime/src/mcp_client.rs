//! Потребление внешних серверов инструментов по MCP (T1).
//!
//! Источник: `docs/arch/stack.md` §6, ADR-0023. ROADMAP: T1.
//!
//! Официальный Rust SDK (`rmcp`) вместо самодельного клиента — ADR-0023
//! явно про использование готового открытого стандарта, не про
//! проектирование собственного протокола/фреймера с нуля.
//!
//! Клиент — `()` (`rmcp::ClientHandler` для `()`): Berimor в этой задаче
//! только потребляет `tools/list`/`tools/call`, не обрабатывает
//! server-initiated запросы (sampling/roots/elicitation) — тот же приём
//! экономии scope, что и везде в этой фазе: механизм, не всё, что
//! протокол умеет.

use rmcp::model::{CallToolRequestParams, CallToolResult, JsonObject, Tool};
use rmcp::service::{RoleClient, RunningService, ServiceError};
use rmcp::transport::{IntoTransport, TokioChildProcess};
use rmcp::ServiceExt;

#[derive(Debug, thiserror::Error)]
pub enum McpClientError {
    #[error("не удалось запустить процесс сервера инструментов: {0}")]
    Spawn(#[source] std::io::Error),
    #[error("не удалось установить MCP-сессию: {0}")]
    Initialize(#[source] Box<rmcp::service::ClientInitializeError>),
    #[error("вызов MCP-сервера завершился ошибкой: {0}")]
    Service(#[source] ServiceError),
    #[error("не удалось корректно завершить MCP-сессию: {0}")]
    Close(#[source] tokio::task::JoinError),
}

impl From<ServiceError> for McpClientError {
    fn from(err: ServiceError) -> Self {
        McpClientError::Service(err)
    }
}

/// Сессия с внешним сервером инструментов по MCP.
pub struct McpClient {
    service: RunningService<RoleClient, ()>,
}

impl McpClient {
    /// Устанавливает MCP-сессию поверх произвольного транспорта —
    /// используется и `spawn` (дочерний процесс по stdio), и тестами
    /// (`tokio::io::duplex` с сервером на другом конце) без дублирования
    /// логики рукопожатия.
    pub async fn connect<T, E, A>(transport: T) -> Result<Self, McpClientError>
    where
        T: IntoTransport<RoleClient, E, A>,
        E: std::error::Error + Send + Sync + 'static,
    {
        let service = ()
            .serve(transport)
            .await
            .map_err(|err| McpClientError::Initialize(Box::new(err)))?;
        Ok(Self { service })
    }

    /// Запускает `command` как дочерний процесс и устанавливает с ним
    /// MCP-сессию по stdio — самый частый случай локального сервера
    /// инструментов (изоляция процесса — предмет T3, здесь только
    /// транспорт).
    pub async fn spawn(command: tokio::process::Command) -> Result<Self, McpClientError> {
        let transport = TokioChildProcess::new(command).map_err(McpClientError::Spawn)?;
        Self::connect(transport).await
    }

    /// Список инструментов, которые отдаёт сервер. Пагинация протокола
    /// (`cursor`) не выведена наружу в этой задаче — вызывающему коду
    /// пока достаточно первой страницы; постраничный обход — расширение
    /// по мере необходимости, не выдуманное здесь заранее.
    pub async fn list_tools(&self) -> Result<Vec<Tool>, McpClientError> {
        let result = self.service.list_tools(None).await?;
        Ok(result.tools)
    }

    /// Вызывает инструмент `name` с аргументами `arguments` (JSON-объект
    /// или `None`, если инструмент их не требует).
    pub async fn call_tool(
        &self,
        name: impl Into<String>,
        arguments: Option<JsonObject>,
    ) -> Result<CallToolResult, McpClientError> {
        let mut params = CallToolRequestParams::new(name.into());
        if let Some(arguments) = arguments {
            params = params.with_arguments(arguments);
        }
        let result = self.service.call_tool(params).await?;
        Ok(result)
    }

    /// Закрывает сессию и дожидается завершения фонового цикла
    /// обработки — для дочернего процесса это ещё и корректная остановка
    /// самого процесса.
    pub async fn close(self) -> Result<(), McpClientError> {
        self.service.cancel().await.map_err(McpClientError::Close)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmcp::handler::server::router::tool::{ToolRoute, ToolRouter};
    use rmcp::handler::server::tool::ToolCallContext;
    use rmcp::model::{
        CallToolResponse, ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerInfo,
    };
    use rmcp::service::RequestContext;
    use rmcp::ErrorData;
    use rmcp::{RoleServer, ServerHandler};
    use serde_json::json;
    use std::sync::Arc;

    /// Сервер-заглушка на другой стороне `tokio::io::duplex` — реальный
    /// MCP-хендшейк и маршрутизация, не мок протокола. Один инструмент
    /// `echo`, который возвращает переданный аргумент как есть, и один
    /// `fail`, который всегда завершается ошибкой протокола — этого
    /// достаточно, чтобы проверить оба пути клиента (`Ok`/`Err`).
    #[derive(Clone)]
    struct StubServer {
        router: ToolRouter<Self>,
    }

    impl StubServer {
        fn new() -> Self {
            let mut router = ToolRouter::new();
            router.add_route(ToolRoute::new_dyn(
                rmcp::model::Tool::new("echo", "Возвращает аргумент как есть", Arc::default()),
                |ctx: ToolCallContext<Self>| {
                    let arguments = ctx.arguments.clone().unwrap_or_default();
                    Box::pin(async move {
                        Ok(CallToolResult::structured(serde_json::Value::Object(arguments)).into())
                    })
                },
            ));
            router.add_route(ToolRoute::new_dyn(
                rmcp::model::Tool::new("fail", "Всегда падает", Arc::default()),
                |_ctx: ToolCallContext<Self>| {
                    Box::pin(async move {
                        Err(ErrorData::internal_error("намеренный сбой теста", None))
                    })
                },
            ));
            Self { router }
        }
    }

    impl ServerHandler for StubServer {
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

    async fn connected_pair() -> (McpClient, tokio::task::JoinHandle<()>) {
        let (server_half, client_half) = tokio::io::duplex(8192);
        let server_task = tokio::spawn(async move {
            let server = StubServer::new().serve(server_half).await.unwrap();
            server.waiting().await.unwrap();
        });
        let client = McpClient::connect(client_half).await.unwrap();
        (client, server_task)
    }

    #[tokio::test]
    async fn list_tools_returns_what_the_server_declares() {
        let (client, server_task) = connected_pair().await;

        let tools = client.list_tools().await.unwrap();

        assert_eq!(tools.len(), 2);
        assert!(tools.iter().any(|t| t.name == "echo"));
        assert!(tools.iter().any(|t| t.name == "fail"));

        client.close().await.unwrap();
        server_task.abort();
    }

    #[tokio::test]
    async fn call_tool_round_trips_structured_arguments() {
        let (client, server_task) = connected_pair().await;

        let mut arguments = JsonObject::new();
        arguments.insert("text".into(), json!("hello"));
        let result = client.call_tool("echo", Some(arguments)).await.unwrap();

        assert_eq!(result.is_error, Some(false));
        assert_eq!(result.structured_content, Some(json!({"text": "hello"})));

        client.close().await.unwrap();
        server_task.abort();
    }

    #[tokio::test]
    async fn call_tool_propagates_protocol_error_from_the_server() {
        let (client, server_task) = connected_pair().await;

        let result = client.call_tool("fail", None).await;

        assert!(matches!(result, Err(McpClientError::Service(_))));

        client.close().await.unwrap();
        server_task.abort();
    }

    #[tokio::test]
    async fn call_tool_for_unknown_name_is_an_error() {
        let (client, server_task) = connected_pair().await;

        let result = client.call_tool("no-such-tool", None).await;

        assert!(matches!(result, Err(McpClientError::Service(_))));

        client.close().await.unwrap();
        server_task.abort();
    }
}
