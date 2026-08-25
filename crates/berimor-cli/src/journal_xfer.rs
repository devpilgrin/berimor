//! Федеративный журнал (волна I, 0.46.0): перенос прогона между машинами
//! без разделения записи (ADR-0028 L1: рой отклонён, перенос — вместо).
//! `berimor journal export <instance> --out <file>`: события инстанса в
//! один JSON с sha256-свёрткой; `berimor journal import <file>`: сверка
//! свёртки, переименование при коллизии id, запись с ИСХОДНЫМИ метками
//! времени (append_preserved). Импорт никогда не дописывает в
//! существующий инстанс: один писатель на инстанс — и на чужой машине.

use std::path::PathBuf;

use berimor_storage::{EventLog, SqliteEventLog};
use berimor_types::event::{Event, ProcessInstanceId};

use crate::config::Config;
use crate::run::RunError;

/// Переносимый конверт журнала.
#[derive(serde::Serialize, serde::Deserialize)]
struct JournalEnvelope {
    format: String,
    version: u32,
    instance: String,
    count: usize,
    sha256: String,
    events: Vec<Event>,
}

const FORMAT: &str = "berimor-journal";

/// Свёртка событий: канонический JSON каждого события по порядку.
fn fingerprint(events: &[Event]) -> String {
    use sha2::Digest;
    let mut hasher = sha2::Sha256::new();
    for event in events {
        let line = serde_json::to_vec(event).expect("event serializes");
        hasher.update(&line);
    }
    hex::encode(hasher.finalize())
}

/// `berimor journal export <instance> --out <file>`.
pub fn export(config: &Config, instance: &str, out: &PathBuf) -> Result<(), RunError> {
    let storage = SqliteEventLog::open(&config.storage_path).map_err(|err| {
        RunError::Gate(format!("журнал {}: {err}", config.storage_path.display()))
    })?;
    let events = storage
        .replay(&ProcessInstanceId(instance.to_string()))
        .map_err(|err| RunError::Gate(format!("journal export: чтение: {err}")))?;
    if events.is_empty() {
        return Err(RunError::Gate(format!(
            "journal export: инстанс '{instance}' не найден или пуст"
        )));
    }
    let envelope = JournalEnvelope {
        format: FORMAT.to_string(),
        version: 1,
        instance: instance.to_string(),
        count: events.len(),
        sha256: fingerprint(&events),
        events,
    };
    let text = serde_json::to_string_pretty(&envelope)
        .map_err(|err| RunError::Gate(format!("journal export: сериализация: {err}")))?;
    std::fs::write(out, text).map_err(|err| {
        RunError::Gate(format!("journal export: запись {}: {err}", out.display()))
    })?;
    println!(
        "[berimor] экспортировано: {count} событий прогона '{instance}' → {path} (sha256 {hash}…)",
        count = envelope.count,
        path = out.display(),
        hash = &envelope.sha256[..12],
    );
    Ok(())
}

/// `berimor journal import <file>`: сверка свёртки, коллизия → суффикс.
pub fn import(config: &Config, file: &PathBuf) -> Result<(), RunError> {
    let text = std::fs::read_to_string(file).map_err(|err| {
        RunError::Gate(format!("journal import: чтение {}: {err}", file.display()))
    })?;
    let envelope: JournalEnvelope = serde_json::from_str(&text).map_err(|err| {
        RunError::Gate(format!("journal import: не конверт berimor-journal: {err}"))
    })?;
    if envelope.format != FORMAT || envelope.version != 1 {
        return Err(RunError::Gate(format!(
            "journal import: неподдерживаемый формат '{}' v{}",
            envelope.format, envelope.version
        )));
    }
    if envelope.count != envelope.events.len() {
        return Err(RunError::Gate(format!(
            "journal import: заявлено {} событий, в файле {}",
            envelope.count,
            envelope.events.len()
        )));
    }
    if fingerprint(&envelope.events) != envelope.sha256 {
        return Err(RunError::Gate(
            "journal import: sha256 не сошёлся — файл повреждён или изменён".into(),
        ));
    }
    let storage = SqliteEventLog::open(&config.storage_path)
        .map_err(|err| RunError::Gate(format!("журнал: {err}")))?;
    // Коллизия id: импорт — НЕ допись в существующий инстанс.
    let existing: std::collections::HashSet<String> = storage
        .list_instance_ids()
        .unwrap_or_default()
        .into_iter()
        .collect();
    let mut target = envelope.instance.clone();
    let mut suffix = 1;
    while existing.contains(&target) {
        suffix += 1;
        target = format!("{}-imported-{suffix}", envelope.instance);
    }
    let mut written = 0usize;
    for mut event in envelope.events {
        event.process_instance = ProcessInstanceId(target.clone());
        storage
            .append_preserved(event)
            .map_err(|err| RunError::Gate(format!("journal import: запись: {err}")))?;
        written += 1;
    }
    println!(
        "[berimor] импортировано: {written} событий → прогон '{target}'{renamed}",
        renamed = if target == envelope.instance {
            String::new()
        } else {
            format!(" (коллизия id: '{}' уже был)", envelope.instance)
        },
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use berimor_types::event::EventKind;

    fn event(seq: u64, ts: i64) -> Event {
        Event {
            seq: berimor_types::event::EventSeq(seq),
            process_instance: ProcessInstanceId("run-test".into()),
            process_version: 1,
            kind: EventKind::Instantiated,
            payload: serde_json::json!({"n": seq}),
            ts_ms: ts,
        }
    }

    #[test]
    fn fingerprint_detects_tamper() {
        let events = vec![event(1, 100), event(2, 200)];
        let hash = fingerprint(&events);
        assert_eq!(hash, fingerprint(&events)); // детерминирована
        let mut tampered = events.clone();
        tampered[0].payload = serde_json::json!({"n": 99});
        assert_ne!(hash, fingerprint(&tampered));
    }

    #[test]
    fn export_import_roundtrip_preserves_ts_and_renames_on_collision() {
        let dir = std::env::temp_dir().join(format!("berimor-jxfer-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let config = Config {
            storage_path: dir.join("journal.db"),
            ..Config::default()
        };
        let out = dir.join("export.json");

        let storage = SqliteEventLog::open(&config.storage_path).expect("open");
        storage.append(event(0, 111)).expect("append");
        storage.append(event(0, 222)).expect("append");
        drop(storage);

        export(&config, "run-test", &out).expect("export");
        // Пауза: если бы импорт штамповал своё now, ts бы разошлись.
        std::thread::sleep(std::time::Duration::from_millis(60));
        // Импорт в ТОТ ЖЕ журнал → коллизия → переименование.
        import(&config, &out).expect("import");
        let storage = SqliteEventLog::open(&config.storage_path).expect("reopen");
        let ids = storage.list_instance_ids().expect("ids");
        assert!(ids.iter().any(|i| i == "run-test"));
        let imported: Vec<_> = ids
            .iter()
            .filter(|i| i.starts_with("run-test-imported"))
            .collect();
        assert_eq!(
            imported.len(),
            1,
            "ожидался ровно один импортированный: {ids:?}"
        );
        let events = storage
            .replay(&ProcessInstanceId(imported[0].clone()))
            .expect("replay");
        assert_eq!(events.len(), 2);
        // Импорт сохраняет ts из конверта (не момент импорта).
        let envelope: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&out).expect("read")).expect("json");
        let exported_ts0 = envelope["events"][0]["ts_ms"].as_i64().expect("ts");
        assert_eq!(events[0].ts_ms, exported_ts0, "ts — из файла, не now");

        // Повреждённая свёртка — отказ (правим содержимое, хеш старый).
        let text = std::fs::read_to_string(&out).expect("read");
        let mut bad: serde_json::Value = serde_json::from_str(&text).expect("json");
        bad["events"][0]["payload"]["n"] = serde_json::json!(424242);
        std::fs::write(&out, serde_json::to_string_pretty(&bad).expect("ser")).expect("write");
        assert!(
            import(&config, &out).is_err(),
            "tampered файл обязан быть отвергнут"
        );
    }
}
