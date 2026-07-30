//! `berimor-tool-runtime` — MCP-клиент/сервер, изоляция плагина.
//!
//! Источник: `ideal-agent-architecture.md` §3.9, ADR-0023.
//!
//! Реализовано: T1 (`mcp_client`) — потребление внешних серверов
//! инструментов по MCP через официальный SDK `rmcp`; T2 (`mcp_server`) —
//! отдача произвольного заданного вызывающим кодом набора инструментов
//! по MCP.

pub mod mcp_client;
pub mod mcp_server;
pub mod plugin_process;
