//! Снапшоты файлов перед мутацией (волна C10, arch §3.9 «снапшоты»,
//! spec docs/rnd/builtin-tools-waves-spec.md).
//!
//! Перед перезаписью существующего файла (files.write, files.edit)
//! диспетчер копирует его в `<workspace>/.berimor/snapshots/<UTC-ts>/`
//! с сохранением относительного пути. Ротация — последние 50 каталогов
//! (старшие удаляются целиком). Файлы больше CONTENT_CAP не снапшотятся
//! (пометка `snapshot: "skipped"` в ответе операции).
//!
//! Инструменты: `snapshot.list` (чтение) и `snapshot.restore`
//! (восстановление, mutates=true). Снапшоты — внутренняя бухгалтерия
//! в .berimor/, не пользовательские данные.

use crate::builtin_dispatch::{err_str, resolve_from, CONTENT_CAP};
use berimor_executors::tool_only::DispatchError;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// Потолок хранимых снапшотов (каталогов-меток); старшие удаляются.
const KEEP: usize = 50;

/// Корень снапшотов рабочей области.
fn snapshots_root(root: &Path) -> PathBuf {
    root.join(".berimor").join("snapshots")
}

/// Метка каталога снапшота: UTC `YYYYMMDD-HHMMSS` + короткий суффикс
/// pid (два снапшота в одну секунду не сталкиваются).
fn snapshot_id() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    // UTC-дата/время из epoch без внешних зависимостей (civil-from-days).
    let days = (now / 86_400) as i64;
    let secs = now % 86_400;
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}{m:02}{d:02}-{:02}{:02}{:02}-{}",
        secs / 3600,
        (secs % 3600) / 60,
        secs % 60,
        std::process::id() % 1000
    )
}

/// Говард Хиннант, civil_from_days (публичный алгоритм, без зависимостей).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Снять снапшот файла ПЕРЕД его перезаписью (вызывается диспетчером
/// из веток files.write/files.edit). Возвращает:
/// - Ok(Some(id)) — снапшот сделан (или уже был в этом каталоге-метке);
/// - Ok(None) — файла не существовало (снапшотить нечего) или он больше
///   капа (пропуск осознанный — операция не блокируется);
/// - Err — сбой ФС (операция записи НЕ должна умирать из-за снапшота:
///   вызывающий логирует в ответ операции, но продолжает).
pub fn take(root: &Path, abs_path: &Path) -> Result<Option<String>, DispatchError> {
    if !abs_path.is_file() {
        return Ok(None);
    }
    let size = std::fs::metadata(abs_path)
        .map_err(|e| {
            err_str(
                "snapshot",
                format!("метаданные '{}': {e}", abs_path.display()),
            )
        })?
        .len();
    if size > CONTENT_CAP {
        return Ok(None);
    }
    let rel = abs_path.strip_prefix(root).unwrap_or(abs_path);
    // Защита от абсолютных путей вне root: складываем внешние под
    // каталог `_outside/` с манглированием разделителей.
    let dest_rel: PathBuf = if rel.is_absolute() {
        PathBuf::from("_outside").join(rel.display().to_string().replace(['/', '\\'], "__"))
    } else {
        rel.to_path_buf()
    };
    let id = snapshot_id();
    let dest_dir = snapshots_root(root).join(&id);
    let dest = dest_dir.join(&dest_rel);
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| err_str("snapshot", format!("каталог снапшота: {e}")))?;
    }
    std::fs::copy(abs_path, &dest)
        .map_err(|e| err_str("snapshot", format!("копия '{}': {e}", abs_path.display())))?;
    rotate(root);
    Ok(Some(id))
}

/// Ротация: оставить последние KEEP каталогов-меток (лексикографический
/// порядок меток = хронологический по формату метки).
fn rotate(root: &Path) {
    let base = snapshots_root(root);
    let Ok(mut entries) = std::fs::read_dir(&base).map(|rd| {
        rd.filter_map(|e| e.ok())
            .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
            .map(|e| e.file_name())
            .collect::<Vec<_>>()
    }) else {
        return;
    };
    entries.sort();
    while entries.len() > KEEP {
        if let Some(oldest) = entries.first() {
            let _ = std::fs::remove_dir_all(base.join(oldest));
        }
        entries.remove(0);
    }
}

/// `snapshot.list` — список меток с содержимым (mutates=false).
pub fn list(root: &Path, args: &Value) -> Result<Value, DispatchError> {
    let limit = args["limit"].as_u64().unwrap_or(20).clamp(1, 200) as usize;
    let base = snapshots_root(root);
    let mut entries: Vec<String> = match std::fs::read_dir(&base) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect(),
        Err(_) => Vec::new(),
    };
    entries.sort();
    entries.reverse(); // свежие первыми
    let items: Vec<Value> = entries
        .into_iter()
        .take(limit)
        .map(|id| {
            let mut paths: Vec<String> = Vec::new();
            collect_files(&base.join(&id), &base.join(&id), &mut paths);
            json!({"id": id, "paths": paths})
        })
        .collect();
    Ok(json!({"snapshots": items}))
}

fn collect_files(dir: &Path, base: &Path, out: &mut Vec<String>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in rd.filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.is_dir() {
            collect_files(&path, base, out);
        } else if let Ok(rel) = path.strip_prefix(base) {
            out.push(rel.display().to_string());
        }
    }
}

/// `snapshot.restore` — восстановить файл(ы) из метки (mutates=true).
pub fn restore(root: &Path, args: &Value) -> Result<Value, DispatchError> {
    let id = args["id"]
        .as_str()
        .ok_or_else(|| err_str("snapshot.restore", "аргумент 'id' обязателен (строка)"))?;
    // Метка — имя каталога: разделители запрещены (выход за корень).
    if id.contains(['/', '\\']) || id.contains("..") {
        return Err(err_str(
            "snapshot.restore",
            "недопустимый id снапшота (разделители/точки)",
        ));
    }
    let src_dir = snapshots_root(root).join(id);
    if !src_dir.is_dir() {
        return Err(err_str(
            "snapshot.restore",
            format!("снапшот '{id}' не найден"),
        ));
    }
    let only_path = args["path"].as_str();
    let mut restored: Vec<String> = Vec::new();
    let mut files: Vec<String> = Vec::new();
    collect_files(&src_dir, &src_dir, &mut files);
    for rel in files {
        if let Some(filter) = only_path {
            if rel != filter {
                continue;
            }
        }
        if rel.starts_with("_outside/") {
            continue; // внешние файлы обратно не восстанавливаем никогда
        }
        let dest = resolve_from(root, &rel);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| err_str("snapshot.restore", format!("каталог назначения: {e}")))?;
        }
        std::fs::copy(src_dir.join(&rel), &dest)
            .map_err(|e| err_str("snapshot.restore", format!("восстановление '{rel}': {e}")))?;
        restored.push(rel);
    }
    if restored.is_empty() {
        return Err(err_str(
            "snapshot.restore",
            match only_path {
                Some(p) => format!("путь '{p}' отсутствует в снапшоте '{id}'"),
                None => format!("снапшот '{id}' пуст"),
            },
        ));
    }
    Ok(json!({"id": id, "restored": restored}))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("berimor-snap-test-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn take_list_restore_roundtrip() {
        let root = temp_dir("roundtrip");
        let file = root.join("note.txt");
        std::fs::write(&file, "старая версия").unwrap();
        let id = take(&root, &file).unwrap().expect("снапшот сделан");
        std::fs::write(&file, "новая версия").unwrap();
        let list = list(&root, &json!({})).unwrap();
        assert_eq!(list["snapshots"][0]["id"].as_str().unwrap(), id);
        let restored = restore(&root, &json!({"id": id})).unwrap();
        assert_eq!(restored["restored"][0].as_str().unwrap(), "note.txt");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "старая версия");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn take_skips_missing_and_restores_only_matching_path() {
        let root = temp_dir("filter");
        assert_eq!(take(&root, &root.join("ghost.txt")).unwrap(), None);
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("a.txt"), "a").unwrap();
        std::fs::write(root.join("sub/b.txt"), "b").unwrap();
        // Два файла в одну метку: второй take в той же секунде делит id.
        let id1 = take(&root, &root.join("a.txt")).unwrap().unwrap();
        let id2 = take(&root, &root.join("sub/b.txt")).unwrap().unwrap();
        std::fs::write(root.join("a.txt"), "a2").unwrap();
        let out = restore(&root, &json!({"id": id1, "path": "a.txt"})).unwrap();
        assert_eq!(out["restored"][0].as_str().unwrap(), "a.txt");
        assert_eq!(std::fs::read_to_string(root.join("a.txt")).unwrap(), "a");
        let _ = id2;
        // Мусорный id — ошибка, не выход за корень.
        assert!(restore(&root, &json!({"id": "../.."})).is_err());
        assert!(restore(&root, &json!({"id": "no-such"})).is_err());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn rotation_keeps_last_fifty() {
        let root = temp_dir("rotate");
        let base = snapshots_root(&root);
        for i in 0..55 {
            let dir = base.join(format!("20200101-0000{i:02}-x"));
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("f.txt"), "x").unwrap();
        }
        std::fs::write(root.join("live.txt"), "v").unwrap();
        take(&root, &root.join("live.txt")).unwrap();
        let count = std::fs::read_dir(&base).unwrap().count();
        assert!(count <= KEEP + 1, "ротация: {count}");
        std::fs::remove_dir_all(&root).ok();
    }
}
