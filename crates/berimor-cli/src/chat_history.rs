//! Персистентная лента чата между сессиями (§20.15, репорт
//! пользователя: «при запуске заново агент не подхватывает контекст
//! прошлых сессий»). Лента — JSONL на РАБОЧУЮ ОБЛАСТЬ (ключ —
//! канонизированный путь): агент в директории проекта помнит её
//! историю, в соседней директории — свою. Журнал SQLite при этом
//! остаётся аудит-следом телеметрии; лента — UX-контекст, не аудит.
//!
//! Формат строки: {"role": "user"|"assistant", "content": "..."} —
//! ровно то, что уходит в state.history агента. Хранится
//! замаскированным (контент уже прошёл Masker до записи — та же
//! политика, что у журнала: секретов в покое нет).

use serde_json::Value;
use std::path::{Path, PathBuf};

/// Глубина подхвата: последние N записей (≈ N/2 ходов). Больше —
/// бессмысленный расход контекста модели на каждый старт.
const RESUME_DEPTH: usize = 40;

fn sessions_dir() -> Option<PathBuf> {
    crate::config::global_dir().map(|dir| dir.join("sessions"))
}

/// Файл ленты рабочей области: sha256 пути, первые 16 hex — читаемо и
/// без коллизий на практике.
fn session_file(workspace: &Path) -> Option<PathBuf> {
    let canonical = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf());
    // Стабильный хэш пути области — тем же механизмом, что id фактов
    // семантической памяти (единая утилита, без новой зависимости).
    let hash =
        berimor_memory::semantic::fact_hash(&canonical.to_string_lossy(), "workspace", "chat");
    sessions_dir().map(|dir| dir.join(format!("{}.jsonl", &hash.to_hex()[..16])))
}

/// Подхват ленты при старте чата: последние RESUME_DEPTH записей
/// области. Пусто — пустой вектор (первая сессия в этой области).
pub fn load(workspace: &Path) -> Vec<Value> {
    let Some(path) = session_file(workspace) else {
        return Vec::new();
    };
    let Ok(contents) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    let entries: Vec<Value> = contents
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();
    let skip = entries.len().saturating_sub(RESUME_DEPTH);
    entries.into_iter().skip(skip).collect()
}

/// Дописывает ход (user + assistant) в ленту области. Сбой записи не
/// хоронит сессию — лента UX-контекст, не аудит: молчаливо пропускаем,
/// но без паники.
pub fn append(workspace: &Path, user: &str, assistant: &str) {
    let Some(path) = session_file(workspace) else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    use std::io::Write as _;
    let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    else {
        return;
    };
    for (role, content) in [("user", user), ("assistant", assistant)] {
        let line = serde_json::json!({"role": role, "content": content});
        let _ = writeln!(file, "{line}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_then_load_round_trip_with_depth_cap() {
        let dir = std::env::temp_dir().join(format!("berimor-hist-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for i in 0..30 {
            append(&dir, &format!("вопрос {i}"), &format!("ответ {i}"));
        }
        let entries = load(&dir);
        // 60 записей записано, подхват — последние 40.
        assert_eq!(entries.len(), RESUME_DEPTH);
        assert_eq!(entries[0]["content"], "вопрос 10");
        assert_eq!(entries[entries.len() - 1]["content"], "ответ 29");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn different_workspaces_have_separate_histories() {
        let a = std::env::temp_dir().join(format!("berimor-hist-a-{}", std::process::id()));
        let b = std::env::temp_dir().join(format!("berimor-hist-b-{}", std::process::id()));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        append(&a, "только в A", "ответ A");
        assert_eq!(load(&a).len(), 2);
        assert!(load(&b).is_empty());
        std::fs::remove_dir_all(&a).ok();
        std::fs::remove_dir_all(&b).ok();
    }
}
