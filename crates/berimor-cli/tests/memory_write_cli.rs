//! Записной путь памяти (memory-model.md §2/§4): e2e-доказательство, что
//! извлечение фактов после Finished реально пишет в семантический слой
//! на пути `berimor run` — не только в юнит-тестах `semantic::resolve`.
//!
//! Мок-провайдер отвечает по очереди: classify → answer →
//! FactProposalBatch (вызов извлечения). Далее проверяется сам журнал:
//! факт записан (с маскировкой и стабильным id), повторное извлечение
//! того же — Duplicate (не плодит копий), конфликт с существующим фактом
//! НЕ перезаписывает его и оставляет событие MemoryConflict.

use berimor_storage::{EventLog, FactRecord, SemanticStore, SqliteEventLog};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::Command;

const GOLDEN_PROCESS: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../fixtures/golden/processes/card-delivery-support.yaml"
);

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_berimor"))
}

/// Мок, отвечающий телами `bodies` по очереди подключений.
fn recording_mock(bodies: Vec<Value>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
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

            let content = serde_json::to_string(&body).unwrap();
            let envelope = json!({"choices": [{"message": {"content": content}}]}).to_string();
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{envelope}",
                envelope.len()
            );
            reader.get_mut().write_all(response.as_bytes()).unwrap();
        }
    });
    url
}

fn classify_body() -> Value {
    json!({"category": "card", "risk": 2, "summary": "Клиент спрашивает о статусе доставки карты."})
}

fn answer_body() -> Value {
    json!({"card_id": "card_1029", "reply": "Ваша карта активна и будет доставлена в срок."})
}

fn extract_body(fact: Value) -> Value {
    json!({"facts": [fact]})
}

fn fact(subject: &str, predicate: &str, object: &str) -> Value {
    json!({
        "subject": subject,
        "predicate": predicate,
        "object": object,
        "confidence": 0.9,
        "source": "berimor run"
    })
}

fn write_config(dir: &std::path::Path, db_name: &str, mock_url: &str) -> PathBuf {
    let config_path = dir.join(format!("{db_name}.toml"));
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

[[tool_stubs]]
tool = "crm.get_card_status"
response = {{status = "in_transit"}}
mutates = false

[memory]
fact_extraction = true
"#
        ),
    )
    .unwrap();
    config_path
}

fn run_cli(dir: &std::path::Path, config_path: &std::path::Path) -> std::process::Output {
    Command::new(bin())
        .arg("--config")
        .arg(config_path)
        .arg("run")
        .arg(GOLDEN_PROCESS)
        .arg("--input")
        .arg(r#"{"user":{"card_id":"card_1029"}}"#)
        .current_dir(dir)
        .output()
        .unwrap()
}

#[test]
fn fact_extraction_writes_fact_after_finished_in_real_cli_run() {
    let dir = std::env::temp_dir().join(format!("berimor-e2e-memw-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let url = recording_mock(vec![
        classify_body(),
        answer_body(),
        extract_body(fact("card_1029", "delivery_status", "in_transit")),
    ]);
    let config_path = write_config(&dir, "memw", &url);

    let output = run_cli(&dir, &config_path);
    assert!(
        output.status.success(),
        "run обязан завершиться: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let storage = SqliteEventLog::open(&dir.join("memw.db")).unwrap();
    let facts = storage.all_facts().unwrap();
    assert_eq!(facts.len(), 1, "ожидался ровно один записанный факт");
    assert_eq!(facts[0].subject, "card_1029");
    assert_eq!(facts[0].predicate, "delivery_status");
    assert_eq!(facts[0].object, "in_transit");
    assert!(facts[0].id.starts_with("f-"), "стабильный id по хэшу");
}

#[test]
fn reextraction_of_same_fact_is_duplicate_not_a_second_record() {
    let dir = std::env::temp_dir().join(format!("berimor-e2e-memdup-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    // Первый прогон — пишет факт.
    let url = recording_mock(vec![
        classify_body(),
        answer_body(),
        extract_body(fact("card_1029", "delivery_status", "in_transit")),
    ]);
    let config_path = write_config(&dir, "memdup", &url);
    let output = run_cli(&dir, &config_path);
    assert!(output.status.success());

    // Второй прогон с тем же извлечением — Duplicate, записей не прибавляется.
    let url = recording_mock(vec![
        classify_body(),
        answer_body(),
        extract_body(fact("card_1029", "delivery_status", "in_transit")),
    ]);
    let config_path = write_config(&dir, "memdup", &url);
    let output = run_cli(&dir, &config_path);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let storage = SqliteEventLog::open(&dir.join("memdup.db")).unwrap();
    assert_eq!(
        storage.all_facts().unwrap().len(),
        1,
        "повторное извлечение того же факта — Duplicate, не вторая запись"
    );
}

#[test]
fn conflicting_fact_is_rejected_and_journaled_not_overwritten() {
    let dir = std::env::temp_dir().join(format!("berimor-e2e-memconf-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    // Существующий факт: тот же субъект и предикат, ДРУГОЙ объект.
    let storage = SqliteEventLog::open(&dir.join("memconf.db")).unwrap();
    storage
        .upsert_fact(
            &FactRecord {
                id: "f-existing".into(),
                subject: "card_1029".into(),
                predicate: "delivery_status".into(),
                object: "delivered".into(),
                confidence: 0.9,
                source: "seed".into(),
                trusted_channel: false,
            },
            None,
        )
        .unwrap();

    let url = recording_mock(vec![
        classify_body(),
        answer_body(),
        extract_body(fact("card_1029", "delivery_status", "in_transit")),
    ]);
    let config_path = write_config(&dir, "memconf", &url);

    let output = run_cli(&dir, &config_path);
    assert!(
        output.status.success(),
        "конфликт не должен хоронить завершённый процесс: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let facts = storage.all_facts().unwrap();
    assert_eq!(facts.len(), 1, "конфликтный кандидат не записывается");
    assert_eq!(
        facts[0].object, "delivered",
        "существующий факт не перезаписывается молча (memory-model.md §2)"
    );

    // Событие конфликта — в журнале инстанса. Id инстанса — из stdout прогона.
    let stdout = String::from_utf8_lossy(&output.stdout);
    let instance_id = stdout
        .split("создан инстанс ")
        .nth(1)
        .and_then(|tail| tail.split_whitespace().next())
        .expect("прогон печатает id инстанса");
    let events = storage
        .replay(&berimor_types::event::ProcessInstanceId(
            instance_id.to_string(),
        ))
        .unwrap();
    assert!(
        events.iter().any(|e| matches!(
            &e.kind,
            berimor_types::event::EventKind::MemoryConflict { .. }
        )),
        "конфликт обязан оставить событие MemoryConflict в журнале"
    );
}

#[test]
fn extraction_disabled_by_default_flag_off_writes_nothing() {
    let dir = std::env::temp_dir().join(format!("berimor-e2e-memoff-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    // Мок отвечает ТОЛЬКО дважды: если извлечение случайно включится —
    // третий запрос повиснет/упадёт, и прогон не будет успешным.
    let url = recording_mock(vec![classify_body(), answer_body()]);
    let config_path = dir.join("memoff.toml");
    std::fs::write(
        &config_path,
        format!(
            r#"storage_path = "./memoff.db"
confirmation_mode = "off"

[[providers]]
name = "mock"
model_id = "mock-model"
tier = "strong"
base_url = "{url}"
api_key = "mock-key"
allow_private_endpoint = true

[[tool_stubs]]
tool = "crm.get_card_status"
response = {{status = "in_transit"}}
mutates = false
"#
        ),
    )
    .unwrap();

    let output = run_cli(&dir, &config_path);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let storage = SqliteEventLog::open(&dir.join("memoff.db")).unwrap();
    assert!(
        storage.all_facts().unwrap().is_empty(),
        "без флага извлечение не выполняется и ничего не пишет"
    );
}
