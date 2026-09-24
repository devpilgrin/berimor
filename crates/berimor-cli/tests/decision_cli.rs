//! Провайдеры решений Jev/Kev через настоящий `berimor run`: golden-процесс
//! `decision-routing.yaml` маршрутизирует по `decision.ask`. Два свойства:
//! 1. с провайдером (мок kev.serve) ветку выбирает `routed`, человек не нужен;
//! 2. без `[[decision_providers]]` тот же процесс не падает — уходит к
//!    человеку через `on_failure: uncertain` и доходит до `Finished`.

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

const PROCESS: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../fixtures/golden/processes/decision-routing.yaml"
);
const TICKET: &str = "С меня дважды списали деньги за подписку, верните переплату";

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_berimor"))
}

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("berimor-decision-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Мок kev.serve на одно соединение: возвращает тело запроса для проверки.
fn mock_kev(response: Value) -> (String, Arc<Mutex<Value>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let captured = Arc::new(Mutex::new(Value::Null));
    let captured_in_thread = Arc::clone(&captured);
    std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream);
        let mut content_length = 0usize;
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if line.to_lowercase().starts_with("content-length:") {
                content_length = line[15..].trim().parse().unwrap();
            }
            if line.trim().is_empty() {
                break;
            }
        }
        let mut body = vec![0u8; content_length];
        reader.read_exact(&mut body).unwrap();
        *captured_in_thread.lock().unwrap() = serde_json::from_slice(&body).unwrap();
        let payload = response.to_string();
        let reply = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{payload}",
            payload.len()
        );
        reader.get_mut().write_all(reply.as_bytes()).unwrap();
    });
    (url, captured)
}

fn write_config(dir: &Path, kev_url: Option<&str>) -> PathBuf {
    let path = dir.join("config.toml");
    let storage = dir
        .join("journal.db")
        .to_string_lossy()
        .replace('\\', "\\\\");
    let decision = kev_url
        .map(|url| {
            format!(
                r#"
[[decision_providers]]
name = "kev"
base_url = "{url}"
model = "kev-latest"
allow_private_endpoint = true
"#
            )
        })
        .unwrap_or_default();
    std::fs::write(
        &path,
        format!(
            r#"
storage_path = "{storage}"
confirmation_mode = "smart"
{decision}
[[tool_stubs]]
tool = "crm.assign_queue"
mutates = false
response = {{ assigned = true }}
"#
        ),
    )
    .unwrap();
    path
}

fn run(config: &Path, stdin_lines: &[&str]) -> (bool, String) {
    let mut child = Command::new(bin())
        .env("XDG_CONFIG_HOME", "/nonexistent-berimor-decision-xdg")
        .arg("--config")
        .arg(config)
        .arg("run")
        .arg(PROCESS)
        .arg("--input")
        .arg(json!({"ticket": {"text": TICKET}}).to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("бинарник berimor собран (cargo test)");
    let mut stdin = child.stdin.take().unwrap();
    for line in stdin_lines {
        writeln!(stdin, "{line}").unwrap();
    }
    drop(stdin);
    let output = child.wait_with_output().unwrap();
    let text = format!(
        "{}\n--- stderr ---\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    (output.status.success(), text)
}

fn final_state(output: &str) -> Value {
    let marker = "[berimor] процесс завершён";
    let idx = output
        .find(marker)
        .unwrap_or_else(|| panic!("процесс не дошёл до Finished:\n{output}"));
    let after = &output[idx + marker.len()..];
    let start = after.find('\n').map(|i| i + 1).unwrap_or(0);
    let end = after.find("\n--- stderr ---").unwrap_or(after.len());
    serde_json::from_str(after[start..end].trim())
        .unwrap_or_else(|err| panic!("финальное состояние не JSON ({err}):\n{output}"))
}

#[test]
fn confident_kev_answer_routes_without_human() {
    let dir = temp_dir("confident");
    let (url, captured) = mock_kev(json!({
        "model": "kev-latest",
        "answers": {"team": {"type": "choice", "choice": "billing", "confidence": 0.9,
                             "probabilities": {"billing": 0.95, "shipping": 0.03, "technical": 0.02}}},
        "latency_ms": 310.0
    }));
    // Пустой stdin: попроси процесс человека — отказ (EOF) завалил бы прогон.
    let (ok, output) = run(&write_config(&dir, Some(&url)), &[]);
    assert!(ok, "прогон упал:\n{output}");

    let state = final_state(&output);
    assert_eq!(state["decide"]["provider"], "kev");
    assert_eq!(state["decide"]["answers"]["team"]["routed"], "billing");
    assert_eq!(state["assign"]["assigned"], true);

    let request = captured.lock().unwrap().clone();
    assert_eq!(
        request["state"], TICKET,
        "шаблон {{state.ticket.text}} разрешён в аргументах"
    );
    assert_eq!(request["model"], "kev-latest");
    assert_eq!(request["questions"]["team"]["type"], "choice");
}

#[test]
fn without_decision_providers_the_same_process_goes_to_human() {
    let dir = temp_dir("absent");
    let (ok, output) = run(&write_config(&dir, None), &["y"]);
    assert!(
        ok,
        "без провайдеров процесс обязан отработать через человека:\n{output}"
    );

    let state = final_state(&output);
    assert_eq!(state["decide"]["degraded"], true);
    assert_eq!(state["decide"]["answers"]["team"]["routed"], "uncertain");
    assert_eq!(state["assign"]["assigned"], true);
}
