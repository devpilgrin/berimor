//! CLI4: e2e через реальный бинарник `berimor` (не через вызов функций
//! напрямую, как в тестах `engine.rs`).
//!
//! Источник: `docs/ROADMAP.md` §18.4 (CLI4), §18.5 (DoD Milestone 1).
//! Мок-провайдер вместо реального (не тратим токены/деньги в CI, сеть в
//! CI непредсказуема — тот же урок, что и `http_provider.rs`).
//!
//! Два свойства DoD:
//! 1. golden-фикстура реально доходит до `Finished` через `berimor run`;
//! 2. прерывание на `human_gate` + `--resume` даёт то же финальное
//!    состояние, что и непрерывный прогон того же сценария — доказано
//!    здесь через настоящий CLI-процесс, не через вызов функций теста
//!    `engine.rs` (то свойство P3 уже доказало на уровне библиотеки).

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Command, Stdio};

const GOLDEN_PROCESS: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../fixtures/golden/processes/card-delivery-support.yaml"
);

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_berimor"))
}

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("berimor-e2e-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Мок OpenAI-совместимого провайдера: принимает соединения строго по
/// порядку `bodies`, каждому отвечает конвертом с сериализованным
/// контрактом в `choices[0].message.content` — тем же форматом, что
/// разбирает `OpenAiCompatibleProvider` (E5). Одно соединение на запрос,
/// как в тестах `http_provider.rs` (`connection: close`).
fn mock_provider(bodies: Vec<Value>) -> (String, std::thread::JoinHandle<()>) {
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

            let content = serde_json::to_string(&body).unwrap();
            let envelope = json!({"choices": [{"message": {"content": content}}]}).to_string();
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{envelope}",
                envelope.len()
            );
            reader.get_mut().write_all(response.as_bytes()).unwrap();
        }
    });
    (url, handle)
}

/// `crm.get_card_status` — read-only заглушка (`mutates = false`): проходит
/// режим `smart` по умолчанию без интерактивного подтверждения.
fn write_config(dir: &std::path::Path, storage_name: &str, provider_url: &str) -> PathBuf {
    let path = dir.join(format!("{storage_name}.toml"));
    let storage = dir.join(format!("{storage_name}.db"));
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

[[tool_stubs]]
tool = "crm.get_card_status"
mutates = false
response = {{ status = "active", card_id = "card_1029" }}
"#,
        storage = storage.to_string_lossy().replace('\\', "\\\\"),
    );
    std::fs::write(&path, contents).unwrap();
    path
}

/// Запускает `berimor run`, пишет заданные строки в stdin (ответы на
/// human_gate) и закрывает его — процесс не должен ждать ввода сверх
/// сценария. Возвращает (успех, stdout).
fn run_cli(config: &std::path::Path, resume: Option<&str>, stdin_lines: &[&str]) -> (bool, String) {
    let mut cmd = Command::new(bin());
    cmd.arg("--config")
        .arg(config)
        .arg("run")
        .arg(GOLDEN_PROCESS)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    match resume {
        Some(id) => {
            cmd.arg("--resume").arg(id);
        }
        None => {
            cmd.arg("--input")
                .arg(r#"{"user": {"card_id": "card_1029"}}"#);
        }
    }
    let mut child = cmd.spawn().expect("бинарник berimor собран (cargo test)");
    let mut stdin = child.stdin.take().unwrap();
    for line in stdin_lines {
        writeln!(stdin, "{line}").unwrap();
    }
    drop(stdin); // EOF: лишние read_line() внутри процесса не блокируются

    let output = child.wait_with_output().unwrap();
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
    )
}

fn extract_instance_id(stdout: &str) -> String {
    let marker = "[berimor] создан инстанс ";
    stdout
        .lines()
        .find_map(|line| line.strip_prefix(marker))
        .unwrap_or_else(|| panic!("не найдена строка с id инстанса:\n{stdout}"))
        .trim()
        .to_string()
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

fn classify_low_risk() -> Value {
    json!({"category": "card", "risk": 2, "summary": "Клиент спрашивает о статусе доставки карты."})
}

fn classify_high_risk() -> Value {
    json!({"category": "debt", "risk": 8, "summary": "Крупная задолженность, высокий риск."})
}

fn answer() -> Value {
    json!({"card_id": "card_1029", "reply": "Ваша карта активна и будет доставлена в срок."})
}

/// DoD §18.5, пункт 2: golden-процесс реально доходит до `Finished` через
/// `berimor run` — низкий риск, human_gate не задействован.
#[test]
fn full_run_through_finished_produces_expected_state() {
    let (url, server) = mock_provider(vec![classify_low_risk(), answer()]);
    let dir = temp_dir("continuous-low-risk");
    let config = write_config(&dir, "run", &url);

    let (ok, stdout) = run_cli(&config, None, &[]);
    server.join().unwrap();

    assert!(ok, "прогон обязан завершиться успехом:\n{stdout}");
    let state = extract_final_state(&stdout);

    assert_eq!(state["user"]["card_id"], "card_1029");
    assert_eq!(state["classify"]["category"], "card");
    assert_eq!(state["classify"]["risk"], 2);
    assert_eq!(state["fetch_card_status"]["status"], "active");
    assert_eq!(state["answer"]["card_id"], "card_1029");
    assert_eq!(
        state["answer"]["reply"],
        "Ваша карта активна и будет доставлена в срок."
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// DoD §18.5, пункт 3: прерывание на `human_gate` и `berimor run --resume`
/// восстанавливают тот же результат, что и непрерывный прогон того же
/// сценария (высокий риск — единственный путь через `human_review` в
/// golden-процессе). Оба варианта используют одинаковые мок-ответы модели,
/// сравнивается итоговое состояние целиком.
#[test]
fn interrupted_run_resumes_to_same_final_state_as_continuous_run() {
    // --- Непрерывный прогон: подтверждение "y" сразу же ---------------
    let (url_a, server_a) = mock_provider(vec![classify_high_risk(), answer()]);
    let dir_a = temp_dir("continuous-high-risk");
    let config_a = write_config(&dir_a, "run", &url_a);
    let (ok_a, stdout_a) = run_cli(&config_a, None, &["y"]);
    server_a.join().unwrap();
    assert!(
        ok_a,
        "непрерывный прогон обязан завершиться успехом:\n{stdout_a}"
    );
    let continuous_state = extract_final_state(&stdout_a);
    std::fs::remove_dir_all(&dir_a).ok();

    // --- Прерванный прогон: первый вызов отклоняет human_gate ---------
    let dir_b = temp_dir("interrupted-high-risk");
    let (url_b1, server_b1) = mock_provider(vec![classify_high_risk()]);
    let config_b1 = write_config(&dir_b, "run", &url_b1);
    let (ok_b1, stdout_b1) = run_cli(&config_b1, None, &["n"]);
    server_b1.join().unwrap();
    assert!(
        !ok_b1,
        "отказ на human_gate обязан завершить процесс ошибкой:\n{stdout_b1}"
    );
    let instance_id = extract_instance_id(&stdout_b1);

    // --- Второй вызов: --resume с тем же журналом, human_gate заново ---
    // (current_step восстанавливается по последнему StepApplied — classify
    // — human_review не персистентен как отдельный шаг до подтверждения;
    // это ожидаемое поведение P3/CLI3, не баг: неразрешённый gate обязан
    // спросить заново, не молча продолжить).
    let (url_b2, server_b2) = mock_provider(vec![answer()]);
    let config_b2 = write_config(&dir_b, "run", &url_b2); // тот же storage_path, новый provider
    let (ok_b2, stdout_b2) = run_cli(&config_b2, Some(&instance_id), &["y"]);
    server_b2.join().unwrap();
    assert!(
        ok_b2,
        "возобновлённый прогон обязан завершиться успехом:\n{stdout_b2}"
    );
    let resumed_state = extract_final_state(&stdout_b2);

    assert_eq!(
        continuous_state, resumed_state,
        "прерванный+возобновлённый прогон обязан дать то же состояние, что и непрерывный"
    );

    std::fs::remove_dir_all(&dir_b).ok();
}
