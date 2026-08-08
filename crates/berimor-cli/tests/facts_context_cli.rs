//! prompt-next-wave.md задача 1: e2e-доказательство, что слой `Facts`
//! реально доходит до модели через СЕМАНТИЧЕСКИЙ (не точный) поиск в
//! реальном прогоне `berimor chat` — факт, записанный напрямую в
//! хранилище с реальным эмбеддингом (BAAI/bge-m3, тот же текст-шаблон,
//! что использует записной путь `run.rs::extract_and_store_facts`),
//! находится по ПЕРИФРАЗИРОВАННОМУ запросу пользователя (RU↔RU и
//! RU↔EN) — мок-провайдер записывает сырое тело первого запроса
//! (системный контекст хода), в нём и проверяется присутствие факта.
//!
//! ВНИМАНИЕ: реальная модель эмбеддингов — до ~2 ГБ весов на первом
//! запуске в `~/.local/share/berimor/embeddings`, CPU-инференс. Не в
//! CI, только вручную:
//!
//! ```sh
//! cargo test -p berimor-cli --features embeddings --test facts_context_cli -- --include-ignored
//! ```

#![cfg(feature = "embeddings")]

use berimor_memory::embeddings::FastEmbedder;
use berimor_storage::{FactRecord, SemanticStore, SqliteEventLog};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_berimor"))
}

/// Мок, отвечающий по очереди `bodies` и сохраняющий текст каждого
/// запроса (тот же приём, что `entity_graph_cli.rs`). Возвращает URL
/// ДО того, как запрос реально пришёл — конфиг CLI пишется этим URL,
/// затем процесс запускается.
fn recording_mock(bodies: Vec<Value>) -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let requests_in_thread = Arc::clone(&requests);
    std::thread::spawn(move || {
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
            requests_in_thread
                .lock()
                .unwrap()
                .push(String::from_utf8_lossy(&request_body).to_string());

            let content = serde_json::to_string(&body).unwrap();
            let envelope = json!({"choices": [{"message": {"content": content}}]}).to_string();
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{envelope}",
                envelope.len()
            );
            reader.get_mut().write_all(response.as_bytes()).unwrap();
        }
    });
    (url, requests)
}

fn finish_turn(reply: &str) -> Value {
    json!({
        "thought": "Отвечаю пользователю.",
        "action": {"kind": "finish", "result": {"reply": reply}}
    })
}

fn write_chat_config(dir: &std::path::Path, name: &str, db_name: &str, mock_url: &str) -> PathBuf {
    let config_path = dir.join(format!("{name}.toml"));
    std::fs::write(
        &config_path,
        format!(
            r#"storage_path = "./{db_name}.db"
confirmation_mode = "off"

[[providers]]
name = "mock"
model_id = "mock-model"
tier = "strong"
base_url = "{mock_url}"
api_key = "mock-key"
allow_private_endpoint = true

[memory]
embeddings = true
"#
        ),
    )
    .unwrap();
    config_path
}

/// Факт с РЕАЛЬНЫМ вектором BGE-M3, записан напрямую в хранилище (не
/// через полный конвейер извлечения — тот с реальными эмбеддингами не
/// предмет ЭТОЙ задачи, конвейер без них уже покрыт
/// `memory_write_cli.rs`). Текст-шаблон эмбеддинга — тот же, что
/// использует `run.rs::extract_and_store_facts` на записи
/// (`"{subject} {predicate} {object}"`) — так тест проверяет реальный
/// контракт, а не выдуманный свой.
fn seed_fact(db_path: &std::path::Path, subject: &str, predicate: &str, object: &str) {
    let storage = SqliteEventLog::open(db_path).unwrap();
    let embedder = FastEmbedder::new();
    let text = format!("{subject} {predicate} {object}");
    let embedding = embedder.embed(&text).expect("инференс эмбеддинга факта");
    storage
        .upsert_fact(
            &FactRecord {
                id: "f-seed-1".into(),
                subject: subject.into(),
                predicate: predicate.into(),
                object: object.into(),
                confidence: 0.9,
                source: "seed:facts_context_cli".into(),
                trusted_channel: true,
            },
            Some(&embedding),
        )
        .unwrap();
}

/// Сессия 2: `berimor chat` с реальным эмбеддером, вопрос — ПЕРИФРАЗА
/// (не точное совпадение слов) факта, записанного в сессии 1. Первый
/// запрос к модели несёт собранный контекст хода — в нём проверяется
/// слой `facts`.
fn ask_and_capture_first_request(dir: &std::path::Path, db_name: &str, message: &str) -> String {
    let (url, requests) = recording_mock(vec![finish_turn("Хорошо.")]);
    let config = write_chat_config(dir, "chat-session", db_name, &url);
    let empty_xdg = std::env::temp_dir().join(format!(
        "berimor-e2e-facts-xdg-{}-{:x}",
        std::process::id(),
        message.len()
    ));
    std::fs::create_dir_all(&empty_xdg).unwrap();
    let mut child = Command::new(bin())
        .arg("--config")
        .arg(&config)
        .arg("chat")
        .current_dir(dir)
        .env("XDG_CONFIG_HOME", &empty_xdg)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(format!("{message}\n").as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "chat обязан завершиться: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let first =
        requests.lock().unwrap().first().cloned().expect(
            "мок обязан был получить хотя бы один запрос — сессия должна была дойти до модели",
        );
    first
}

#[test]
#[ignore = "скачивает ~2 ГБ весов BGE-M3 при первом запуске; запуск: --include-ignored"]
fn facts_layer_finds_paraphrased_fact_ru_to_ru() {
    let dir = std::env::temp_dir().join(format!("berimor-e2e-facts-ruru-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    seed_fact(&dir.join("facts.db"), "клиент c-1", "живёт_в", "Москва");

    let body = ask_and_capture_first_request(&dir, "facts", "Где живёт клиент c-1?");

    assert!(
        body.contains("Москва"),
        "факт обязан быть найден по перифразе (RU↔RU): {body}"
    );
}

#[test]
#[ignore = "скачивает ~2 ГБ весов BGE-M3 при первом запуске; запуск: --include-ignored"]
fn facts_layer_finds_paraphrased_fact_ru_to_en() {
    let dir = std::env::temp_dir().join(format!("berimor-e2e-facts-ruen-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    seed_fact(&dir.join("facts.db"), "клиент c-1", "живёт_в", "Москва");

    let body = ask_and_capture_first_request(&dir, "facts", "Where does client c-1 live?");

    assert!(
        body.contains("Москва"),
        "факт обязан быть найден по кросс-языковой перифразе (RU↔EN): {body}"
    );
}

/// Отрицательный контроль: несвязанный вопрос НЕ должен подтягивать
/// факт (иначе тест выше не доказывал бы отбор по релевантности, только
/// «слой всегда непуст»).
#[test]
#[ignore = "скачивает ~2 ГБ весов BGE-M3 при первом запуске; запуск: --include-ignored"]
fn facts_layer_does_not_surface_unrelated_fact() {
    let dir = std::env::temp_dir().join(format!("berimor-e2e-facts-unrel-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    seed_fact(&dir.join("facts.db"), "клиент c-1", "живёт_в", "Москва");

    let body =
        ask_and_capture_first_request(&dir, "facts", "Расскажи рецепт борща на четыре порции");

    assert!(
        !body.contains("Москва"),
        "несвязанный запрос не должен подтягивать факт про клиента: {body}"
    );
}
