//! E8: e2e через реальный бинарник `berimor` для `codeact`-шага — тот же
//! приём мок-провайдера, что и `e2e_run.rs`/`agent_step_cli.rs`
//! (детерминированные HTTP-ответы, без сети/токенов в CI). В отличие от
//! них — раздача сырого ТЕКСТА (JS-программы), не JSON-объекта: модель
//! у CodeAct отвечает программой, не JSON по схеме контракта напрямую
//! (`executors.md` §4.1).

use serde_json::Value;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Command, Stdio};

const GOLDEN_PROCESS: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../fixtures/golden/processes/codeact-support.yaml"
);

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_berimor"))
}

fn temp_dir(name: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("berimor-codeact-e2e-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Тот же мок-сервер, что `e2e_run.rs`/`agent_step_cli.rs`, но раздаёт
/// СЫРОЙ ТЕКСТ ответа как есть (без промежуточной JSON-сериализации
/// тела) — content провайдера обязан быть буквально JS-текстом
/// программы, не JSON-представлением объекта.
fn mock_provider(bodies: Vec<&'static str>) -> (String, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let handle = std::thread::spawn(move || {
        for body in bodies {
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
            let mut request_body = vec![0u8; content_length];
            reader.read_exact(&mut request_body).unwrap();

            let envelope =
                serde_json::json!({"choices": [{"message": {"content": body}}]}).to_string();
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{envelope}",
                envelope.len()
            );
            reader.get_mut().write_all(response.as_bytes()).unwrap();
        }
    });
    (url, handle)
}

fn write_config(dir: &std::path::Path, provider_url: &str) -> PathBuf {
    let path = dir.join("config.toml");
    let storage = dir.join("run.db");
    let contents = format!(
        r#"
storage_path = "{storage}"
confirmation_mode = "smart"

[[providers]]
name = "mock"
model_id = "mock-model"
tier = "weak"
base_url = "{provider_url}"
allow_private_endpoint = true
"#,
        storage = storage.to_string_lossy().replace('\\', "\\\\"),
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

/// DoD по образцу CLI4/e2e_run.rs/agent_step_cli.rs: golden-процесс с
/// `codeact`-шагом реально доходит до `Finished` через `berimor run` —
/// модель отвечает JS-текстом, он проходит статический анализ (E7),
/// исполняется в песочнице (E6/E8, реальный QuickJS-гость), финальный
/// результат — через тот же путь Mediation, что и `llm_structured`/
/// `agent_step` (сфабрикованный `card_id` отклонился бы так же, как в
/// их собственных тестах — здесь используется настоящий `state.user.card_id`).
#[test]
fn codeact_reaches_finished_through_a_real_js_program() {
    let program = "finish({card_id: input.user.card_id, reply: 'Ваша карта активна.'})";
    let (url, server) = mock_provider(vec![program]);
    let dir = temp_dir("basic");
    let config = write_config(&dir, &url);

    let output = Command::new(bin())
        .arg("--config")
        .arg(&config)
        .arg("run")
        .arg(GOLDEN_PROCESS)
        .arg("--input")
        .arg(r#"{"user": {"card_id": "card_1029"}}"#)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("бинарник berimor собран (cargo test)");
    server.join().unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        output.status.success(),
        "прогон обязан завершиться успехом:\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );

    let state = extract_final_state(&stdout);
    assert_eq!(state["answer"]["card_id"], "card_1029");
    assert_eq!(state["answer"]["reply"], "Ваша карта активна.");

    std::fs::remove_dir_all(&dir).ok();
}
