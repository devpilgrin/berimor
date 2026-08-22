//! berimor как MCP-сервер (0.37.0; по мотивам Harness AI MCP Server —
//! «дать внешним агентам доступ к платформе»): внешний агент (Claude
//! Code, Cursor, любой MCP-клиент) гоняет ПРОЦЕССЫ berimor как
//! детерминированный исполнительный контур — модель думает снаружи,
//! код решает внутри.
//!
//! Транспорт: stdio, NDJSON (одна строка = одно JSON-RPC сообщение,
//! MCP stdio). Инструменты наружу:
//! - process.list — процессы (*.yaml с ключом `process:`) в рабочей
//!   директории;
//! - process.run {path, input?} — неинтерактивный запуск через текущий
//!   бинарник (гейты и медиация внутри, как всегда; подтверждения в
//!   non-interactive = отказ → эскалация по правилам процесса);
//! - trace.read — след последнего прогона (berimor trace).
//!
//! Осознанно НЕ наружу: файловые/терминальные инструменты (внешний
//! агент имеет свои), каталоги, конфиг. Поверхность — процессы.

use std::io::{BufRead, Write};

use serde_json::{json, Value};

const PROTOCOL_VERSION: &str = "2024-11-05";

/// Описание инструментов (tools/list).
fn tools() -> Value {
    json!([
        {
            "name": "process.list",
            "description": "Список процессов berimor (YAML с ключом process:) в рабочей директории сервера",
            "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false }
        },
        {
            "name": "process.run",
            "description": "Запуск процесса berimor неинтерактивно: контракты, медиация и гейты внутри. Возвращает вывод прогона",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "путь к YAML процесса" },
                    "input": { "type": "string", "description": "JSON входа процесса (строкой)", "default": "{}" }
                },
                "required": ["path"],
                "additionalProperties": false
            }
        },
        {
            "name": "trace.read",
            "description": "След последнего прогона (ходы, медиация, гейты) из журнала berimor",
            "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false }
        }
    ])
}

/// Разбор одного JSON-RPC сообщения. None — ответ не нужен (нотификация).
pub(crate) fn handle_message(request: &Value) -> Option<Value> {
    let id = request.get("id").cloned();
    let method = request.get("method").and_then(Value::as_str).unwrap_or("");
    match method {
        "initialize" => Some(json!({
            "jsonrpc": "2.0",
            "id": id.unwrap_or(Value::Null),
            "result": {
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "berimor", "version": env!("CARGO_PKG_VERSION") }
            }
        })),
        m if m.starts_with("notifications/") => None,
        "ping" => Some(json!({ "jsonrpc": "2.0", "id": id.unwrap_or(Value::Null), "result": {} })),
        "tools/list" => Some(json!({
            "jsonrpc": "2.0",
            "id": id.unwrap_or(Value::Null),
            "result": { "tools": tools() }
        })),
        "tools/call" => {
            let params = request.get("params").cloned().unwrap_or(json!({}));
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            let result = call_tool(name, &args);
            Some(json!({
                "jsonrpc": "2.0",
                "id": id.unwrap_or(Value::Null),
                "result": result
            }))
        }
        _ => Some(json!({
            "jsonrpc": "2.0",
            "id": id.unwrap_or(Value::Null),
            "error": { "code": -32601, "message": format!("метод '{method}' не поддерживается") }
        })),
    }
}

/// Результат tools/call: content-блоки MCP; isError при сбое.
fn tool_text(text: String, is_error: bool) -> Value {
    json!({
        "content": [{ "type": "text", "text": text }],
        "isError": is_error
    })
}

fn call_tool(name: &str, args: &Value) -> Value {
    match name {
        "process.list" => match list_processes() {
            Ok(list) => tool_text(list, false),
            Err(e) => tool_text(e, true),
        },
        "process.run" => {
            let Some(path) = args.get("path").and_then(Value::as_str) else {
                return tool_text("аргумент 'path' обязателен".into(), true);
            };
            // Граница: процесс из РАБОЧЕЙ директории сервера (jail, как у
            // файловых инструментов): никаких ../ и абсолютных путей.
            let p = std::path::Path::new(path);
            if p.is_absolute() || path.contains("..") {
                return tool_text(
                    "path: только относительный путь внутри рабочей директории".into(),
                    true,
                );
            }
            if !p.exists() {
                return tool_text(format!("процесс не найден: {path}"), true);
            }
            let input = args.get("input").and_then(Value::as_str).unwrap_or("{}");
            run_self(&["run", path, "--input", input, "--non-interactive"])
        }
        "trace.read" => run_self(&["trace"]),
        _ => tool_text(format!("неизвестный инструмент '{name}'"), true),
    }
}

/// Процессы рабочей директории: *.yaml/*.yml с ключом `process:`.
fn list_processes() -> Result<String, String> {
    let mut found: Vec<String> = Vec::new();
    let entries = std::fs::read_dir(".").map_err(|e| e.to_string())?;
    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        let is_yaml = path
            .extension()
            .is_some_and(|ext| ext == "yaml" || ext == "yml");
        if !is_yaml {
            continue;
        }
        if let Ok(content) = std::fs::read_to_string(&path) {
            if content.lines().any(|l| l.starts_with("process:")) {
                let name = content
                    .lines()
                    .find_map(|l| l.strip_prefix("process:").map(str::trim))
                    .unwrap_or("?");
                found.push(format!("{} ({})", name, path.display()));
            }
        }
    }
    if found.is_empty() {
        Ok("процессов нет (ни одного *.yaml с ключом process: в рабочей директории)".into())
    } else {
        Ok(found.join("\n"))
    }
}

/// Запуск собственного бинарника подкомандой (наследуем конфиг,
/// гейты, журнал — всё штатное). Вывод ограничен хвостом.
fn run_self(argv: &[&str]) -> Value {
    let exe = match std::env::current_exe() {
        Ok(e) => e,
        Err(e) => return tool_text(format!("current_exe: {e}"), true),
    };
    let output = std::process::Command::new(exe)
        .args(argv)
        .stdin(std::process::Stdio::null())
        .output();
    match output {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            let stderr = String::from_utf8_lossy(&out.stderr);
            let tail: String = stdout
                .chars()
                .rev()
                .take(12_000)
                .collect::<String>()
                .chars()
                .rev()
                .collect();
            let err_tail: String = stderr
                .chars()
                .rev()
                .take(4_000)
                .collect::<String>()
                .chars()
                .rev()
                .collect();
            let text = if err_tail.trim().is_empty() {
                tail
            } else {
                format!("{tail}\n\n[stderr]\n{err_tail}")
            };
            tool_text(text, !out.status.success())
        }
        Err(e) => tool_text(format!("запуск berimor: {e}"), true),
    }
}

/// Цикл сервера: NDJSON из stdin → ответы в stdout (нотификации без
/// ответа). EOF stdin — выход.
pub(crate) fn serve() -> i32 {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let request: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => {
                let _ = writeln!(
                    stdout,
                    "{}",
                    json!({"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":format!("невалидный JSON: {e}")}})
                );
                let _ = stdout.flush();
                continue;
            }
        };
        if let Some(response) = handle_message(&request) {
            let _ = writeln!(stdout, "{response}");
            let _ = stdout.flush();
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initialize_and_tools_list() {
        let init =
            handle_message(&json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}))
                .expect("ответ");
        assert_eq!(init["result"]["serverInfo"]["name"], "berimor");
        let list = handle_message(&json!({"jsonrpc":"2.0","id":2,"method":"tools/list"})).unwrap();
        let names: Vec<&str> = list["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["process.list", "process.run", "trace.read"]);
        // Нотификации — без ответа.
        assert!(
            handle_message(&json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
                .is_none()
        );
        // Неизвестный метод — -32601.
        let err =
            handle_message(&json!({"jsonrpc":"2.0","id":9,"method":"resources/list"})).unwrap();
        assert_eq!(err["error"]["code"], -32601);
    }

    #[test]
    fn process_run_rejects_escape_paths() {
        let res = handle_message(&json!({
            "jsonrpc":"2.0","id":3,"method":"tools/call",
            "params":{"name":"process.run","arguments":{"path":"../outside.yaml"}}
        }))
        .unwrap();
        assert_eq!(res["result"]["isError"], true);
        assert!(res["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("относительный путь"));
        let res = handle_message(&json!({
            "jsonrpc":"2.0","id":4,"method":"tools/call",
            "params":{"name":"process.run","arguments":{"path":"/etc/passwd.yaml"}}
        }))
        .unwrap();
        assert_eq!(res["result"]["isError"], true);
    }
}
