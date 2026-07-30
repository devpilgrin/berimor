//! T1: настоящий путь «дочерний процесс + stdio» (`McpClient::spawn`).
//!
//! Отдельный интеграционный тест, не юнит-тест в `mcp_client.rs` —
//! `CARGO_BIN_EXE_<name>` (путь к скомпилированному тестовому серверу
//! `src/bin/mcp_stdio_echo_server.rs`) доступен через `env!` только в
//! `tests/`, не в юнит-тестах `src/`.

use berimor_tool_runtime::mcp_client::McpClient;
use serde_json::json;

#[tokio::test]
async fn spawn_starts_a_real_child_process_and_talks_mcp_over_its_stdio() {
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_mcp_stdio_echo_server"));
    command.kill_on_drop(true);
    let client = McpClient::spawn(command).await.unwrap();

    let tools = client.list_tools().await.unwrap();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, "echo");

    let mut arguments = serde_json::Map::new();
    arguments.insert("greeting".into(), json!("привет"));
    let result = client.call_tool("echo", Some(arguments)).await.unwrap();
    assert_eq!(
        result.structured_content,
        Some(json!({"greeting": "привет"}))
    );

    client.close().await.unwrap();
}
