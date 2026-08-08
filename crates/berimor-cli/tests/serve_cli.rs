//! prompt-next-wave.md задача 2: e2e-доказательство `berimor serve`
//! через РЕАЛЬНЫЙ бинарник — unit-тесты auth/routing/bind живут в
//! `src/serve.rs` (in-process, быстрее), здесь — то, что требует
//! настоящего отдельного процесса: реальный побочный эффект
//! `files.write` в рабочей области подпроцесса (`current_dir`), которую
//! in-process тест не мог бы безопасно занять (общий для параллельных
//! тестов корень пакета).

use serde_json::json;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_berimor"))
}

const PROCESS: &str = r#"
process: serve-run-demo
version: 1
steps:
  - id: write_note
    type: tool
    tool: files.write
    args: {path: "note.txt", content: "from serve"}
limits:
  max_steps: 10
  timeout: 1m
"#;

struct ServeGuard(Child);

impl Drop for ServeGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn write_config(dir: &std::path::Path, name: &str, token_env: &str) -> PathBuf {
    let config_path = dir.join(format!("{name}.toml"));
    std::fs::write(
        &config_path,
        format!(
            r#"storage_path = "./{name}.db"
confirmation_mode = "off"

[serve]
token_env = "{token_env}"
"#
        ),
    )
    .unwrap();
    config_path
}

/// `berimor serve --port 0` — ОС выбирает свободный порт (та же логика,
/// что уже проверена unit-тестами `bind`); реальный порт печатается в
/// stderr процесса при старте, разбираем строку.
fn spawn_serve(dir: &std::path::Path, config: &std::path::Path) -> (ServeGuard, u16) {
    let mut child = Command::new(bin())
        .arg("--config")
        .arg(config)
        .arg("serve")
        .arg("--port")
        .arg("0")
        .current_dir(dir)
        .env("XDG_CONFIG_HOME", "/nonexistent-berimor-e2e-serve-xdg")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut reader = BufReader::new(child.stderr.take().unwrap());
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .expect("serve обязан напечатать строку о старте перед accept-циклом");
    let port: u16 = line
        .trim()
        .rsplit(':')
        .next()
        .and_then(|tail| tail.split_whitespace().next())
        .and_then(|p| p.parse().ok())
        .unwrap_or_else(|| panic!("не удалось разобрать порт из строки старта: {line:?}"));
    // Дочитывает остаток stderr в фоне — иначе дочерний процесс
    // заблокируется на переполненном пайпе, как только напечатает
    // достаточно диагностики (несколько запросов дальше по тесту).
    std::thread::spawn(move || {
        let mut sink = String::new();
        let _ = reader.read_to_string(&mut sink);
    });
    (ServeGuard(child), port)
}

fn http_request(port: u16, request: &str) -> String {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream.write_all(request.as_bytes()).unwrap();
    stream.shutdown(Shutdown::Write).ok();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    response
}

#[test]
fn run_endpoint_executes_process_with_real_side_effect_in_subprocess_workspace() {
    let dir = std::env::temp_dir().join(format!("berimor-e2e-serve-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("demo.yaml"), PROCESS).unwrap();
    let token_env = format!("BERIMOR_E2E_SERVE_TOKEN_{}", std::process::id());
    // Safety: тест-only переменная окружения, имя уникально на процесс.
    unsafe {
        std::env::set_var(&token_env, "e2e-secret");
    }
    let config = write_config(&dir, "serve", &token_env);

    let (_guard, port) = spawn_serve(&dir, &config);

    let body = json!({"process": "demo.yaml"}).to_string();
    let request = format!(
        "POST /run HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer e2e-secret\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let response = http_request(port, &request);

    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(response.contains("\"finished\""), "{response}");
    assert!(
        dir.join("note.txt").exists(),
        "процесс обязан реально выполниться в рабочей области дочернего процесса (files.write)"
    );
}

#[test]
fn request_without_token_is_rejected_by_real_subprocess() {
    let dir = std::env::temp_dir().join(format!("berimor-e2e-serve-noauth-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let token_env = format!("BERIMOR_E2E_SERVE_TOKEN_NOAUTH_{}", std::process::id());
    unsafe {
        std::env::set_var(&token_env, "real-secret");
    }
    let config = write_config(&dir, "serve", &token_env);

    let (_guard, port) = spawn_serve(&dir, &config);

    let response = http_request(
        port,
        "GET /sessions HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
    );

    assert!(response.starts_with("HTTP/1.1 401"), "{response}");
}
