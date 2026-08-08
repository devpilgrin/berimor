//! prompt-next-wave.md задача 3: e2e-доказательство `berimor memory
//! consolidate` через реальный бинарник с реальной моделью эмбеддингов
//! (BAAI/bge-m3) — юнит-тесты с фейковым эмбеддером живут в
//! `src/memory.rs` (быстрые, доказывают саму логику слияния/удаления/
//! журналирования); здесь — что вся цепочка (конфиг → CLI → реальный
//! `FastEmbedder` → консолидация) реально работает end-to-end.
//!
//! ВНИМАНИЕ: как и `facts_context_cli.rs` — до ~2 ГБ весов модели на
//! первом запуске. Не в CI:
//!
//! ```sh
//! cargo test -p berimor-cli --features embeddings --test memory_consolidate_cli -- --include-ignored
//! ```

#![cfg(feature = "embeddings")]

use berimor_storage::{EventLog, FactRecord, SemanticStore, SqliteEventLog};
use berimor_types::event::ProcessInstanceId;
use std::path::PathBuf;
use std::process::Command;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_berimor"))
}

fn write_config(dir: &std::path::Path) -> PathBuf {
    let config_path = dir.join("consolidate.toml");
    std::fs::write(
        &config_path,
        r#"storage_path = "./facts.db"

[memory]
embeddings = true
"#,
    )
    .unwrap();
    config_path
}

#[test]
#[ignore = "скачивает ~2 ГБ весов BGE-M3 при первом запуске; запуск: --include-ignored"]
fn consolidate_merges_paraphrased_facts_via_real_cli_run() {
    let dir = std::env::temp_dir().join(format!("berimor-e2e-consolidate-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let db_path = dir.join("facts.db");
    let storage = SqliteEventLog::open(&db_path).unwrap();
    storage
        .upsert_fact(
            &FactRecord {
                id: "f-1".into(),
                subject: "клиент c-1".into(),
                predicate: "живёт_в".into(),
                object: "Москва".into(),
                confidence: 0.6,
                source: "seed".into(),
                trusted_channel: true,
            },
            None,
        )
        .unwrap();
    storage
        .upsert_fact(
            &FactRecord {
                id: "f-2".into(),
                subject: "клиент c-1".into(),
                predicate: "проживает в городе".into(),
                object: "г. Москва".into(),
                confidence: 0.8,
                source: "seed".into(),
                trusted_channel: true,
            },
            None,
        )
        .unwrap();
    // Несвязанный факт — не должен пострадать.
    storage
        .upsert_fact(
            &FactRecord {
                id: "f-3".into(),
                subject: "проект".into(),
                predicate: "написан_на".into(),
                object: "Rust".into(),
                confidence: 0.9,
                source: "seed".into(),
                trusted_channel: true,
            },
            None,
        )
        .unwrap();
    drop(storage);

    let config = write_config(&dir);
    let output = Command::new(bin())
        .arg("--config")
        .arg(&config)
        .arg("memory")
        .arg("consolidate")
        .current_dir(&dir)
        .env(
            "XDG_CONFIG_HOME",
            "/nonexistent-berimor-e2e-consolidate-xdg",
        )
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let storage = SqliteEventLog::open(&db_path).unwrap();
    let facts = storage.all_facts().unwrap();
    assert_eq!(
        facts.len(),
        2,
        "f-1/f-2 — перифраза одного факта, обязаны слиться в одну запись; f-3 остаётся отдельно: {facts:?}"
    );
    assert!(
        facts.iter().any(|f| f.object == "Rust"),
        "несвязанный факт не должен пострадать"
    );
    let merged_survivor = facts
        .iter()
        .find(|f| f.object.contains("Москва"))
        .expect("survivor факта о Москве обязан остаться");
    assert_eq!(
        merged_survivor.confidence, 0.8,
        "уверенность survivor — максимум из слитых (merge_confidence)"
    );

    // "memory-consolidation" — синтетический ProcessInstanceId
    // (`memory.rs::CONSOLIDATION_INSTANCE_ID`); `berimor-cli` — bin-only
    // крейта, интеграционный тест не импортирует внутренние константы,
    // сверяется с задокументированным значением напрямую.
    let events = storage
        .replay(&ProcessInstanceId("memory-consolidation".to_string()))
        .unwrap();
    assert_eq!(events.len(), 1, "слияние обязано быть журналировано");
}
