//! CLI-M2: e2e через реальный бинарник `berimor` с настоящим дочерним
//! MCP-процессом (`[[mcp_servers]]`), не через unit-тесты `mcp_dispatch.rs`
//! напрямую — те проверяют только маршрутизацию `CompositeToolDispatch`
//! поверх уже готового клиента, не путь `command`/`args` → `tokio::process`
//! → рукопожатие → `tools/list` → `tools/call`, который и есть новизна
//! этого инкремента.

use serde_json::Value;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

const GOLDEN_PROCESS: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../fixtures/golden/processes/mcp-echo.yaml"
);

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_berimor"))
}

fn echo_server_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_cli_mcp_echo_server"))
}

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("berimor-mcp-e2e-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_config(dir: &std::path::Path) -> PathBuf {
    let path = dir.join("config.toml");
    let storage = dir.join("run.db");
    let contents = format!(
        r#"
storage_path = "{storage}"
confirmation_mode = "smart"

[[mcp_servers]]
name = "cli-echo"
command = "{command}"
args = []
"#,
        storage = storage.to_string_lossy().replace('\\', "\\\\"),
        command = echo_server_bin().to_string_lossy().replace('\\', "\\\\"),
    );
    std::fs::write(&path, contents).unwrap();
    path
}

fn extract_final_state(stdout: &str) -> Value {
    let marker = "[berimor] процесс завершён";
    let idx = stdout
        .find(marker)
        .unwrap_or_else(|| panic!("процесс не дошёл до Finished:\n{stdout}"));
    let after = &stdout[idx + marker.len()..];
    let json_start = after.find('\n').map(|i| i + 1).unwrap_or(0);
    serde_json::from_str(after[json_start..].trim())
        .unwrap_or_else(|err| panic!("финальное состояние не JSON ({err}):\n{stdout}"))
}

/// Инструмент `tool`-шага реально приходит не из `tool_stubs`, а из
/// настоящего MCP-сервера, поднятого как дочерний процесс из конфига.
/// `cli_echo` не объявлен в `tool_stubs`, поэтому у capability-гейта нет
/// для него политики `mutates` — пессимистичный `true` по умолчанию
/// (`tool_only::execute`) требует подтверждения в режиме `smart`, как и
/// для любого необъявленного инструмента; отвечаем "y", это не обходной
/// путь мимо MCP, а ожидаемое поведение самого capability-слоя (S4).
#[test]
fn tool_step_is_served_by_a_real_mcp_server_not_a_static_stub() {
    let dir = temp_dir("basic");
    let config = write_config(&dir);

    let mut child = Command::new(bin())
        .arg("--config")
        .arg(&config)
        .arg("run")
        .arg(GOLDEN_PROCESS)
        .arg("--input")
        .arg(r#"{"user": {"text": "hello-mcp"}}"#)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("XDG_CONFIG_HOME", "/nonexistent-berimor-e2e-xdg") // изоляция от глобального конфига (§20.12)
        .spawn()
        .expect("бинарник berimor собран (cargo test)");
    writeln!(child.stdin.take().unwrap(), "y").unwrap();

    let output = child.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        output.status.success(),
        "прогон обязан завершиться успехом:\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );

    let state = extract_final_state(&stdout);
    assert_eq!(state["greet"]["text"], "hello-mcp");

    std::fs::remove_dir_all(&dir).ok();
}
