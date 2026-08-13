//! Инструменты `todo.read` / `todo.write` — волна B5 спецификации
//! `docs/rnd/builtin-tools-waves-spec.md`: рабочий список задач агента,
//! хранилище `<root>/.berimor/todo.json` (каталог создаётся при записи).
//!
//! Контракт: `todo.write` — ПОЛНАЯ ЗАМЕНА списка с валидацией
//! (status строго из перечня, не более одного `in_progress`, id непустые
//! и уникальные); `todo.read` — `{items}`, отсутствие файла — пустой
//! список, а не ошибка. JSON собирается и разбирается вручную через
//! [`serde_json::Value`], без serde-derive.
//!
//! mutates: **false**. Обоснование: `todo.json` — внутренняя бухгалтерия
//! агента в служебном каталоге `.berimor/` (как `chat_history`), а не
//! пользовательские данные workspace; capability-гейт пропускает такие
//! инструменты без вопроса. Регистрация в `builtin_dispatch`
//! (BUILTIN_TOOLS/ветки call/политика) — задача родителя, здесь только
//! чистые функции.

use berimor_executors::tool_only::DispatchError;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::builtin_dispatch::err_str;

/// Имена инструментов — фигурируют в DispatchError.tool.
const READ_TOOL: &str = "todo.read";
const WRITE_TOOL: &str = "todo.write";

/// Допустимые значения поля `status` элемента списка (контракт B5).
const VALID_STATUSES: [&str; 4] = ["pending", "in_progress", "completed", "cancelled"];

/// Путь хранилища: `<root>/.berimor/todo.json`.
fn storage_path(root: &Path) -> PathBuf {
    root.join(".berimor").join("todo.json")
}

/// Читает список задач. Отсутствие файла — `{items: []}`, не ошибка:
/// свежий workspace ещё не вёл бухгалтерию. Битый JSON — ошибка, иначе
/// молча «забытие» списка маскировало бы порчу хранилища.
/// allow(dead_code) — до интеграции родителем (ветка в builtin_dispatch),
/// по образцу pub(crate)-хелперов resolve_from/err_str; убрать с первым
/// потребителем.
pub fn read(root: &Path) -> Result<Value, DispatchError> {
    let path = storage_path(root);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(json!({ "items": [] }));
        }
        Err(e) => {
            return Err(err_str(
                READ_TOOL,
                format!("не удалось прочитать '{}': {e}", path.display()),
            ));
        }
    };
    let value: Value = serde_json::from_str(&text).map_err(|e| {
        err_str(
            READ_TOOL,
            format!("файл '{}' не валидный JSON: {e}", path.display()),
        )
    })?;
    let items = value["items"].as_array().ok_or_else(|| {
        err_str(
            READ_TOOL,
            format!(
                "файл '{}': поле 'items' отсутствует или не массив",
                path.display()
            ),
        )
    })?;
    Ok(json!({ "items": items }))
}

/// Полная замена списка задач. `args`: `{items: [{id, content, status}]}`.
/// Валидация до любой записи на диск: status из перечня (ошибка с именем
/// поля), не более одного `in_progress`, id непустые и уникальные.
/// Ответ — нормализованный `{items}` (только контрактные поля), тот же
/// JSON ложится в хранилище, так что write→read — точный круг.
/// allow(dead_code) — до интеграции родителем, см. [`read`].
pub fn write(root: &Path, args: &Value) -> Result<Value, DispatchError> {
    let items = args["items"]
        .as_array()
        .ok_or_else(|| err_str(WRITE_TOOL, "аргумент 'items' обязателен (массив)"))?;
    // LOW #17 ревью 2026-08-13: капы — как CONTENT_CAP у остальных
    // инструментов (список — служебный, а не склад данных).
    const MAX_ITEMS: usize = 100;
    const MAX_CONTENT_CHARS: usize = 4096;
    if items.len() > MAX_ITEMS {
        return Err(err_str(
            WRITE_TOOL,
            format!(
                "не более {MAX_ITEMS} элементов списка (получено {})",
                items.len()
            ),
        ));
    }
    let mut seen_ids: HashSet<&str> = HashSet::with_capacity(items.len());
    let mut normalized = Vec::with_capacity(items.len());
    for item in items {
        let id = item["id"]
            .as_str()
            .ok_or_else(|| err_str(WRITE_TOOL, "элемент списка: поле 'id' обязательно (строка)"))?;
        if id.is_empty() {
            return Err(err_str(
                WRITE_TOOL,
                "элемент списка: поле 'id' не может быть пустым",
            ));
        }
        if !seen_ids.insert(id) {
            return Err(err_str(WRITE_TOOL, format!("дубликат id '{id}'")));
        }
        let content = item["content"].as_str().ok_or_else(|| {
            err_str(
                WRITE_TOOL,
                format!("элемент '{id}': поле 'content' обязательно (строка)"),
            )
        })?;
        if content.chars().count() > MAX_CONTENT_CHARS {
            return Err(err_str(
                WRITE_TOOL,
                format!("элемент '{id}': content длиннее {MAX_CONTENT_CHARS} символов"),
            ));
        }
        let status = item["status"].as_str().ok_or_else(|| {
            err_str(
                WRITE_TOOL,
                format!("элемент '{id}': поле 'status' обязательно (строка)"),
            )
        })?;
        if !VALID_STATUSES.contains(&status) {
            return Err(err_str(
                WRITE_TOOL,
                format!(
                    "элемент '{id}': неверное значение поля 'status' — '{status}' \
                     (допустимо: {})",
                    VALID_STATUSES.join(", ")
                ),
            ));
        }
        normalized.push(json!({
            "id": id,
            "content": content,
            "status": status,
        }));
    }
    // «Не более одного in_progress» — по нормализованному списку, чтобы
    // не зависеть от порядка проверок выше.
    let in_progress = normalized
        .iter()
        .filter(|item| item["status"] == "in_progress")
        .count();
    if in_progress > 1 {
        return Err(err_str(
            WRITE_TOOL,
            format!("не более одного элемента со status 'in_progress' (получено {in_progress})"),
        ));
    }
    let dir = root.join(".berimor");
    std::fs::create_dir_all(&dir).map_err(|e| {
        err_str(
            WRITE_TOOL,
            format!("не удалось создать каталог '{}': {e}", dir.display()),
        )
    })?;
    let path = storage_path(root);
    // to_string для Value не падает (все значения уже в памяти) — but map_err
    // на всякий случай оставляем говорящим.
    let text = serde_json::to_string_pretty(&json!({ "items": normalized }))
        .map_err(|e| err_str(WRITE_TOOL, format!("не удалось сериализовать список: {e}")))?;
    std::fs::write(&path, format!("{text}\n")).map_err(|e| {
        err_str(
            WRITE_TOOL,
            format!("не удалось записать '{}': {e}", path.display()),
        )
    })?;
    Ok(json!({ "items": normalized }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Temp-каталог по конвенции berimor-<mod>-test-<tag>-<pid>
    /// (tag различает тесты модуля — гонка temp-каталогов при
    /// параллельном прогоне).
    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("berimor-todo-test-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn write_then_read_round_trip() {
        let dir = temp_dir("roundtrip");
        let args = json!({
            "items": [
                {"id": "1", "content": "Разобрать спеку", "status": "completed"},
                {"id": "2", "content": "Написать модуль", "status": "in_progress"},
                {"id": "3", "content": "Прогнать проверки", "status": "pending"},
            ]
        });
        let written = write(&dir, &args).unwrap();
        assert_eq!(written["items"].as_array().unwrap().len(), 3);
        // Каталог хранилища создан, файл на месте.
        assert!(dir.join(".berimor").join("todo.json").exists());
        let read_back = read(&dir).unwrap();
        assert_eq!(read_back, written);
        assert_eq!(read_back["items"][1]["status"], "in_progress");
        // Полная замена: второй write стирает первый список.
        let rewritten = write(
            &dir,
            &json!({"items": [{"id": "x", "content": "единственная", "status": "pending"}]}),
        )
        .unwrap();
        assert_eq!(rewritten["items"].as_array().unwrap().len(), 1);
        assert_eq!(read(&dir).unwrap(), rewritten);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn invalid_status_errors_and_names_the_field() {
        let dir = temp_dir("badstatus");
        let err = write(
            &dir,
            &json!({"items": [{"id": "1", "content": "задача", "status": "doing"}]}),
        )
        .unwrap_err();
        assert!(err.reason.contains("'status'"), "{}", err.reason);
        assert!(err.reason.contains("doing"), "{}", err.reason);
        // Файл не создан при невалидном вводе.
        assert!(!dir.join(".berimor").join("todo.json").exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn two_in_progress_items_error() {
        let dir = temp_dir("twoprogress");
        let err = write(
            &dir,
            &json!({
                "items": [
                    {"id": "1", "content": "одна", "status": "in_progress"},
                    {"id": "2", "content": "другая", "status": "in_progress"},
                ]
            }),
        )
        .unwrap_err();
        assert!(err.reason.contains("in_progress"), "{}", err.reason);
        assert!(!dir.join(".berimor").join("todo.json").exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn duplicate_id_errors() {
        let dir = temp_dir("dupeid");
        let err = write(
            &dir,
            &json!({
                "items": [
                    {"id": "1", "content": "одна", "status": "pending"},
                    {"id": "1", "content": "другая", "status": "pending"},
                ]
            }),
        )
        .unwrap_err();
        assert!(err.reason.contains("дубликат id '1'"), "{}", err.reason);
        assert!(!dir.join(".berimor").join("todo.json").exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn read_missing_file_is_empty_list_not_error() {
        let dir = temp_dir("emptyread");
        let result = read(&dir).unwrap();
        assert_eq!(result, json!({"items": []}));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn cyrillic_content_round_trip() {
        let dir = temp_dir("cyrillic");
        let args = json!({
            "items": [
                {"id": "задача-1", "content": "Проверить кириллицу: привет, мир!", "status": "in_progress"},
                {"id": "задача-2", "content": "Ёжик и «ёлочные» кавычки", "status": "cancelled"},
            ]
        });
        write(&dir, &args).unwrap();
        let read_back = read(&dir).unwrap();
        assert_eq!(
            read_back["items"][0]["content"],
            "Проверить кириллицу: привет, мир!"
        );
        assert_eq!(read_back["items"][1]["id"], "задача-2");
        // Файл на диске — читаемый UTF-8 JSON с кириллицей как есть.
        let raw = std::fs::read_to_string(dir.join(".berimor").join("todo.json")).unwrap();
        assert!(raw.contains("Ёжик"), "{raw}");
        std::fs::remove_dir_all(&dir).ok();
    }
}
