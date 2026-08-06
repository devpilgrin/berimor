//! §20.22: execution-режим акторов — расписания и демон в CLI.
//!
//! Слой berimor-actors/berimor-storage (персистентный планировщик,
//! защита от двойного тика, скачок после простоя — аудит 4.4) реализован
//! и протестирован изолированно; этот модуль подключает его к командам:
//!
//! - `berimor schedule add <process.yaml> --every 10m [--input JSON]` —
//!   повторяющееся расписание; `--once-in 5m` — одноразовое;
//! - `berimor schedule list` / `berimor schedule remove <id>`;
//! - `berimor daemon [--once] [--tick-cap 1m]` — цикл исполнения:
//!   due-расписания → прогон процесса существующим executor-bundle →
//!   следующий тик. `--once` — один тик и выход (для cron/ручного
//!   запуска). Журнал общий с `berimor run` — срабатывания видны в
//!   `berimor trace` как обычные инстансы.
//!
//! Payload расписания: `{"process_path": ..., "input": {...}}` —
//! детерминированный контракт, не свободная команда: демон исполняет
//! процессы, не шелл-строки (та же модель доверия, что у `berimor run`).
//! Ошибка прогона не останавливает демон: срабатывание уже потреблено
//! тиком (без двойного огня), сбой виден в журнале инстанса.

use berimor_storage::{Schedule, ScheduleId, ScheduleStore, SqliteEventLog};

use crate::config::Config;
use crate::run;

/// Открывает журнал рабочей области (тот же файл, что `berimor run`).
fn open_storage(config: &Config) -> Result<SqliteEventLog, String> {
    SqliteEventLog::open(&config.storage_path).map_err(|err| {
        format!(
            "не удалось открыть журнал {}: {err}",
            config.storage_path.display()
        )
    })
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// `schedule add`: `--every <dur>` (повторяющееся) или `--once-in <dur>`
/// (одноразовое) — ровно один из двух, длительность с суффиксом
/// (`parse_duration_seconds`: 30s/10m/1h — контракт 1.13 аудита).
pub fn schedule_add(
    config: &Config,
    process_path: &str,
    every: &Option<String>,
    once_in: &Option<String>,
    input: &Option<String>,
) -> Result<(), String> {
    if every.is_some() == once_in.is_some() {
        return Err("укажите ровно один режим: --every <dur> (повторяющееся) или --once-in <dur> (одноразовое)".to_string());
    }
    if !std::path::Path::new(process_path).exists() {
        return Err(format!("файл процесса не найден: {process_path}"));
    }
    let input_value: serde_json::Value = match input {
        Some(text) => {
            serde_json::from_str(text).map_err(|err| format!("--input не валидный JSON: {err}"))?
        }
        None => serde_json::json!({}),
    };
    let now = now_ms();
    let (interval_ms, next_fire_ms) = match (every, once_in) {
        (Some(dur), None) => {
            let secs = berimor_types::parser_support::parse_duration_seconds(dur)
                .map_err(|err| format!("--every: {err}"))?;
            let interval = (secs as i64) * 1000;
            (Some(interval), now + interval)
        }
        (None, Some(dur)) => {
            let secs = berimor_types::parser_support::parse_duration_seconds(dur)
                .map_err(|err| format!("--once-in: {err}"))?;
            (None, now + (secs as i64) * 1000)
        }
        _ => unreachable!("проверено выше"),
    };

    let id = ScheduleId(format!(
        "sched-{}-{}",
        std::path::Path::new(process_path)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("process"),
        now
    ));
    let schedule = Schedule {
        id: id.clone(),
        next_fire_ms,
        interval_ms,
        payload: serde_json::json!({
            "process_path": process_path,
            "input": input_value,
        }),
    };
    let storage = open_storage(config)?;
    // Валидация interval > 0 — на слое хранилища (аудит 4.4), здесь
    // дополнительно видимая оператору ошибка, не молчаливая запись.
    storage
        .upsert_schedule(&schedule)
        .map_err(|err| format!("расписание отклонено хранилищем: {err}"))?;
    println!(
        "добавлено расписание {} (процесс: {process_path}, следующее срабатывание через {} мс{})",
        id.0,
        next_fire_ms - now,
        interval_ms
            .map(|i| format!(", повтор каждые {} мс", i))
            .unwrap_or_else(|| ", одноразовое".to_string())
    );
    Ok(())
}

/// `schedule list`: все расписания по ближайшему срабатыванию.
pub fn schedule_list(config: &Config) -> Result<(), String> {
    let storage = open_storage(config)?;
    let schedules = storage
        .list_schedules()
        .map_err(|err| format!("не удалось прочитать расписания: {err}"))?;
    if schedules.is_empty() {
        println!("расписаний нет");
        return Ok(());
    }
    let now = now_ms();
    for sched in schedules {
        let process = sched.payload["process_path"].as_str().unwrap_or("?");
        let in_ms = sched.next_fire_ms - now;
        println!(
            "{}  процесс={}  через={}мс  {}",
            sched.id.0,
            process,
            in_ms,
            sched
                .interval_ms
                .map(|i| format!("повтор={i}мс"))
                .unwrap_or_else(|| "одноразовое".to_string())
        );
    }
    Ok(())
}

/// `schedule remove <id>`: снять расписание.
pub fn schedule_remove(config: &Config, id: &str) -> Result<(), String> {
    let storage = open_storage(config)?;
    storage
        .cancel_schedule(&ScheduleId(id.to_string()))
        .map_err(|err| format!("не удалось снять расписание {id}: {err}"))?;
    println!("расписание {id} снято");
    Ok(())
}

/// `berimor daemon`: цикл тиков. Каждый тик атомарно забирает due
/// (защита от двойного срабатывания — storage tick, не наш код), прогоняет
/// процесс каждого сработавшего расписания тем же путём, что `berimor run`
/// (общий журнал, общий executor-bundle, общие гейты).
pub fn run_daemon(config: &Config, once: bool, tick_cap_ms: i64) -> Result<(), String> {
    let storage = open_storage(config)?;
    eprintln!(
        "[berimor] демон запущен (журнал: {}{})",
        config.storage_path.display(),
        if once { ", один тик" } else { "" }
    );
    loop {
        let fired = storage
            .tick(now_ms())
            .map_err(|err| format!("тик планировщика: {err}"))?;
        for schedule in &fired {
            let process_path = schedule.payload["process_path"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            let input = schedule.payload["input"].to_string();
            eprintln!(
                "[berimor] расписание {}: запуск процесса {process_path}",
                schedule.id.0
            );
            // Прогон — обычный путь berimor run: instantiate → цикл →
            // Finished/Failed в общем журнале. Ошибка — видна в журнале
            // и stderr, демон продолжает (не умирает от одного сбоя).
            if let Err(err) = run::run(config, &process_path, &None, &Some(input)) {
                eprintln!(
                    "[berimor] расписание {}: прогон завершился ошибкой: {err}",
                    schedule.id.0
                );
            }
        }
        if once {
            if fired.is_empty() {
                eprintln!("[berimor] тик: due-расписаний нет");
            }
            return Ok(());
        }
        // Сон до ближайшего срабатывания, но не дольше потолка тика —
        // свежедобавленные расписания подхватываются без рестарта.
        let sleep_ms = storage
            .list_schedules()
            .ok()
            .and_then(|all| all.first().map(|s| s.next_fire_ms - now_ms()))
            .unwrap_or(tick_cap_ms)
            .clamp(250, tick_cap_ms);
        std::thread::sleep(std::time::Duration::from_millis(sleep_ms as u64));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_config() -> (Config, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("berimor-daemon-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let config = Config {
            storage_path: dir.join(format!("{}.db", std::process::id())),
            ..Config::default()
        };
        (config, dir)
    }

    #[test]
    fn schedule_add_rejects_both_modes_and_neither() {
        let (config, _dir) = temp_config();
        let both = schedule_add(
            &config,
            "p.yaml",
            &Some("5m".into()),
            &Some("5m".into()),
            &None,
        );
        assert!(both.is_err());
        let neither = schedule_add(&config, "p.yaml", &None, &None, &None);
        assert!(neither.is_err());
    }

    #[test]
    fn schedule_add_rejects_missing_process_file() {
        let (config, _dir) = temp_config();
        let result = schedule_add(&config, "no-such.yaml", &Some("5m".into()), &None, &None);
        assert!(result.unwrap_err().contains("не найден"));
    }

    #[test]
    fn schedule_add_then_list_then_remove_roundtrip() {
        let (config, dir) = temp_config();
        let process = dir.join("noop.yaml");
        std::fs::write(&process, "process: noop\nversion: 1\nsteps: []\n").unwrap();
        schedule_add(
            &config,
            process.to_str().unwrap(),
            &Some("5m".into()),
            &None,
            &Some("{\"k\": 1}".into()),
        )
        .unwrap();
        let storage = SqliteEventLog::open(&config.storage_path).unwrap();
        let all = storage.list_schedules().unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].payload["process_path"], process.to_str().unwrap());
        assert_eq!(all[0].payload["input"]["k"], 1);
        assert_eq!(all[0].interval_ms, Some(300_000));

        schedule_remove(&config, &all[0].id.0).unwrap();
        assert!(storage.list_schedules().unwrap().is_empty());
        // Журнал при этом жив — события не пострадали.
        let _ = storage;
    }

    #[test]
    fn schedule_add_rejects_invalid_duration_and_bad_json() {
        let (config, dir) = temp_config();
        let process = dir.join("noop.yaml");
        std::fs::write(&process, "process: noop\nversion: 1\nsteps: []\n").unwrap();
        let bad_dur = schedule_add(
            &config,
            process.to_str().unwrap(),
            &Some("5x".into()),
            &None,
            &None,
        );
        assert!(bad_dur.is_err());
        let bad_json = schedule_add(
            &config,
            process.to_str().unwrap(),
            &Some("5m".into()),
            &None,
            &Some("not-json".into()),
        );
        assert!(bad_json.unwrap_err().contains("JSON"));
    }
}
