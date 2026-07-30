//! `berimor-tool-runtime` — MCP-клиент/сервер, изоляция плагина.
//!
//! Источник: `ideal-agent-architecture.md` §3.9, ADR-0023.
//!
//! Реализовано (Фаза 8, полностью): T1 (`mcp_client`) — потребление
//! внешних серверов инструментов по MCP через официальный SDK `rmcp`;
//! T2 (`mcp_server`) — отдача произвольного заданного вызывающим кодом
//! набора инструментов по MCP; T3 (`plugin_process`) — плагин как
//! изолированный процесс (T1) + применение ACL-манифеста (S6,
//! `berimor_capability::plugin::PluginManifest`) к его вызовам.

pub mod mcp_client;
pub mod mcp_server;
pub mod plugin_process;
