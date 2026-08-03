//! §20.5: e2e-доказательство, что слой графа сущностей реально доходит
//! до модели на пути `berimor run` — не только в юнит-тестах построителя.
//!
//! Мок-провайдер ЗАПИСЫВАЕТ тела запросов: системный контекст первого
//! вызова (classify) обязан содержать релевантный узел графа, его соседа
//! по ребру и само ребро. Граф сидится в тот же SQLite-журнал, из
//! которого бежит процесс, — тем самым `EntityGraphStore`, что читает
//! `MemoryContextBuilder` при `[memory] entity_graph = true`.

use berimor_storage::{EdgeRecord, EntityGraphStore, NodeRecord, SqliteEventLog};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex};

const GOLDEN_PROCESS: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../fixtures/golden/processes/card-delivery-support.yaml"
);

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_berimor"))
}

/// Мок, отвечающий по очереди `bodies` и сохраняющий текст каждого
/// запроса — по нему проверяется, что дошло до модели.
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

#[test]
fn entity_graph_layer_reaches_the_model_in_real_cli_run() {
    let dir = std::env::temp_dir().join(format!("berimor-e2e-graph-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let storage_path = dir.join("graph.db");
    let storage = SqliteEventLog::open(&storage_path).unwrap();
    storage
        .upsert_node(&NodeRecord {
            id: "card_1029".into(),
            node_type: "card".into(),
            properties: json!({"holder": "Иван"}),
        })
        .unwrap();
    storage
        .upsert_node(&NodeRecord {
            id: "batch_77".into(),
            node_type: "batch".into(),
            properties: json!({"status": "issued"}),
        })
        .unwrap();
    storage
        .upsert_edge(&EdgeRecord {
            id: "e1".into(),
            edge_type: "issued_in".into(),
            source: "card_1029".into(),
            target: "batch_77".into(),
            properties: json!({}),
        })
        .unwrap();
    drop(storage);

    let (url, requests) = recording_mock(vec![
        json!({"category": "card", "risk": 2, "summary": "статус доставки"}),
        json!({"card_id": "card_1029", "reply": "Карта активна."}),
    ]);
    let config_path = dir.join("berimor.toml");
    std::fs::write(
        &config_path,
        format!(
            r#"
storage_path = "{storage}"
confirmation_mode = "smart"

[memory]
entity_graph = true

[[providers]]
name = "mock"
model_id = "mock-model"
tier = "strong"
base_url = "{url}"
allow_private_endpoint = true

[[tool_stubs]]
tool = "crm.get_card_status"
mutates = false
response = {{ status = "active", card_id = "card_1029" }}
"#,
            storage = storage_path.to_string_lossy().replace('\\', "\\\\"),
        ),
    )
    .unwrap();

    let output = Command::new(bin())
        .arg("--config")
        .arg(&config_path)
        .arg("run")
        .arg(GOLDEN_PROCESS)
        .arg("--input")
        .arg(r#"{"user":{"card_id":"card_1029"}}"#)
        .current_dir(&dir)
        .env("XDG_CONFIG_HOME", "/nonexistent-berimor-e2e-xdg") // изоляция от глобального конфига (§20.12)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "прогон обязан завершиться: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let bodies = requests.lock().unwrap();
    assert_eq!(bodies.len(), 2, "два вызова модели: classify и answer");
    let classify_request = &bodies[0];
    // Узел, релевантный состоянию, его сосед по ребру и само ребро —
    // в контексте модели, не только в БД.
    assert!(classify_request.contains("card_1029"), "{classify_request}");
    assert!(classify_request.contains("batch_77"), "{classify_request}");
    assert!(classify_request.contains("issued_in"), "{classify_request}");
}

/// Контроль: без `[memory] entity_graph` (профиль по умолчанию выключен,
/// memory-model.md §4) тот же сидированный граф до модели НЕ доходит.
#[test]
fn entity_graph_layer_stays_out_when_not_enabled() {
    let dir = std::env::temp_dir().join(format!("berimor-e2e-graph-off-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let storage_path = dir.join("graph.db");
    let storage = SqliteEventLog::open(&storage_path).unwrap();
    storage
        .upsert_node(&NodeRecord {
            id: "card_1029".into(),
            node_type: "card".into(),
            properties: json!({}),
        })
        .unwrap();
    storage
        .upsert_node(&NodeRecord {
            id: "batch_77".into(),
            node_type: "batch".into(),
            properties: json!({}),
        })
        .unwrap();
    drop(storage);

    let (url, requests) = recording_mock(vec![
        json!({"category": "card", "risk": 2, "summary": "статус"}),
        json!({"card_id": "card_1029", "reply": "ok"}),
    ]);
    let config_path = dir.join("berimor.toml");
    std::fs::write(
        &config_path,
        format!(
            r#"
storage_path = "{storage}"
confirmation_mode = "smart"

[[providers]]
name = "mock"
model_id = "mock-model"
tier = "strong"
base_url = "{url}"
allow_private_endpoint = true

[[tool_stubs]]
tool = "crm.get_card_status"
mutates = false
response = {{ status = "active", card_id = "card_1029" }}
"#,
            storage = storage_path.to_string_lossy().replace('\\', "\\\\"),
        ),
    )
    .unwrap();

    let output = Command::new(bin())
        .arg("--config")
        .arg(&config_path)
        .arg("run")
        .arg(GOLDEN_PROCESS)
        .arg("--input")
        .arg(r#"{"user":{"card_id":"card_1029"}}"#)
        .current_dir(&dir)
        .env("XDG_CONFIG_HOME", "/nonexistent-berimor-e2e-xdg") // изоляция от глобального конфига (§20.12)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let bodies = requests.lock().unwrap();
    assert!(
        !bodies[0].contains("batch_77"),
        "слой выключен, а сосед по ребру всё равно дошёл: {}",
        bodies[0]
    );
}
