//! Инструмент `session.search` — волна C9 спецификации
//! `docs/rnd/builtin-tools-waves-spec.md`: подстрочный поиск по лентам
//! сессий (`*.jsonl` в каталоге `config::global_dir()/sessions`, формат
//! строки — см. `chat_history.rs`: `{"role": "user"|"assistant",
//! "content": "..."}`, опционально `ts`).
//!
//! Контракт: args `{query: string, limit?: 20, role?: "user"|"assistant"}`;
//! совпадение — подстрока по `content`, регистронезависимо (regex НЕ
//! требуется); ответ `{matches: [{file, role, ts, excerpt}]}`, excerpt —
//! ±60 символов вокруг совпадения. Битая строка jsonl пропускается (лента
//! — UX-контекст, не аудит), отсутствие каталога — пустой результат, не
//! ошибка. Регистрация в `builtin_dispatch` — задача родителя, здесь
//! только чистая функция.
//!
//! mutates: **false** — только чтение лент, пользовательские данные и
//! внутреннее хранилище не изменяются.

// Проводка (mod + ветка в диспетчере) — клей родителя; до неё публичная
// функция модуля задействована только в тестах (прецедент builtin_human).
#![allow(dead_code)]

use berimor_executors::tool_only::DispatchError;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

use crate::builtin_dispatch::err_str;

/// Имя инструмента — фигурирует в DispatchError.tool.
const TOOL: &str = "session.search";

/// Лимит совпадений по умолчанию (контракт C9).
const DEFAULT_LIMIT: usize = 20;

/// Радиус excerpt в СИМВОЛАХ вокруг совпадения (контракт C9; не байты —
/// кириллица в UTF-8 многобайтова).
const EXCERPT_RADIUS: usize = 60;

/// Точка входа инструмента. `sessions_dir` передаёт родитель
/// (`config::global_dir()/sessions`); отсутствие каталога — пустой ответ.
pub fn call(sessions_dir: &Path, args: &Value) -> Result<Value, DispatchError> {
    let query = args["query"]
        .as_str()
        .ok_or_else(|| err_str(TOOL, "аргумент 'query' обязателен (строка)"))?;
    if query.is_empty() {
        return Err(err_str(TOOL, "аргумент 'query' не должен быть пустым"));
    }
    let limit = match args.get("limit") {
        None => DEFAULT_LIMIT,
        Some(v) => v
            .as_u64()
            .ok_or_else(|| err_str(TOOL, "аргумент 'limit' должен быть положительным числом"))?
            as usize,
    };
    // Явный ноль — ноль совпадений: проверка лимита стоит ПОСЛЕ push
    // (регрессия первой версии), поэтому ноль отсекаем здесь.
    if limit == 0 {
        return Ok(json!({ "matches": [] }));
    }
    // LOW #13 ревью 2026-08-13: потолок лимита — как MAX_LIMIT у
    // files.search/web.search (ответ не раздувается без контроля).
    const MAX_LIMIT: usize = 200;
    let limit = limit.min(MAX_LIMIT);
    let role_filter = match args.get("role") {
        None => None,
        Some(v) => {
            let role = v
                .as_str()
                .ok_or_else(|| err_str(TOOL, "аргумент 'role' должен быть строкой"))?;
            match role {
                "user" | "assistant" => Some(role),
                _ => {
                    return Err(err_str(
                        TOOL,
                        "аргумент 'role' должен быть \"user\" или \"assistant\"",
                    ));
                }
            }
        }
    };

    if !sessions_dir.is_dir() {
        return Ok(json!({ "matches": [] }));
    }

    // Сортировка имён — детерминированный порядок ответа (read_dir порядок
    // не гарантирует, а limit режет по порядку обхода).
    let mut files: Vec<PathBuf> = std::fs::read_dir(sessions_dir)
        .map_err(|e| {
            err_str(
                TOOL,
                format!(
                    "не удалось прочитать каталог '{}': {e}",
                    sessions_dir.display()
                ),
            )
        })?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "jsonl"))
        .collect();
    files.sort();

    let needle = query.to_lowercase();
    let mut matches = Vec::new();
    'files: for path in files {
        // Нечитаемый файл пропускаем — как и битую строку: лента
        // UX-контекст, частичный ответ полезнее отказа. LOW #13 ревью
        // 2026-08-13: чтение с капом CONTENT_CAP (ленты растут
        // бесконечно), не весь файл в память без потолка.
        let Ok(text) = crate::builtin_dispatch::read_string_capped(&path) else {
            continue;
        };
        let file = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        for line in text.lines() {
            let Ok(entry) = serde_json::from_str::<Value>(line) else {
                continue; // битая строка jsonl — пропуск (контракт C9)
            };
            let role = entry["role"].as_str().unwrap_or_default();
            if role_filter.is_some_and(|want| role != want) {
                continue;
            }
            let Some(content) = entry["content"].as_str() else {
                continue;
            };
            let lower = content.to_lowercase();
            let Some(byte_pos) = lower.find(&needle) else {
                continue;
            };
            matches.push(json!({
                "file": file,
                "role": role,
                "ts": entry.get("ts").cloned().unwrap_or(Value::Null),
                "excerpt": make_excerpt(&lower, byte_pos, needle.chars().count(), content),
            }));
            if matches.len() >= limit {
                break 'files;
            }
        }
    }
    Ok(json!({ "matches": matches }))
}

/// Excerpt ±EXCERPT_RADIUS символов вокруг совпадения. Позиция считается
/// в chars строки нижнего регистра, срез — skip/take по chars исходника
/// (без байтовых срезов: паника на границе UTF-8 исключена; для
/// экзотических раскладок Unicode, меняющих длину при case-fold, допустим
/// сдвиг на символ — excerpt эвристический).
fn make_excerpt(lower: &str, byte_pos: usize, match_chars: usize, content: &str) -> String {
    let match_start = lower[..byte_pos].chars().count();
    let start = match_start.saturating_sub(EXCERPT_RADIUS);
    let len = match_start + match_chars + EXCERPT_RADIUS - start;
    content.chars().skip(start).take(len).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Temp-каталог по конвенции berimor-<mod>-test-<tag>-<pid>
    /// (tag различает тесты модуля — гонка temp-каталогов при
    /// параллельном прогоне).
    fn temp_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("berimor-ses-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_session(dir: &Path, name: &str, lines: &str) {
        std::fs::write(dir.join(name), lines).unwrap();
    }

    #[test]
    fn finds_by_substring_case_insensitive() {
        let dir = temp_dir("substr");
        write_session(
            &dir,
            "a.jsonl",
            concat!(
                "{\"role\":\"user\",\"content\":\"Hello World example\"}\n",
                "{\"role\":\"assistant\",\"content\":\"nothing here\"}\n",
            ),
        );
        let result = call(&dir, &json!({"query": "hello"})).unwrap();
        let matches = result["matches"].as_array().unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0]["file"], "a.jsonl");
        assert_eq!(matches[0]["role"], "user");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn role_filter_narrows_matches() {
        let dir = temp_dir("role");
        write_session(
            &dir,
            "a.jsonl",
            concat!(
                "{\"role\":\"user\",\"content\":\"секрет про кота\"}\n",
                "{\"role\":\"assistant\",\"content\":\"секрет про пса\"}\n",
            ),
        );
        let result = call(&dir, &json!({"query": "секрет", "role": "user"})).unwrap();
        let matches = result["matches"].as_array().unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0]["role"], "user");
        assert_eq!(matches[0]["excerpt"], "секрет про кота");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn limit_caps_matches_across_files() {
        let dir = temp_dir("limit");
        write_session(
            &dir,
            "a.jsonl",
            concat!(
                "{\"role\":\"user\",\"content\":\"игла один\"}\n",
                "{\"role\":\"assistant\",\"content\":\"игла два\"}\n",
            ),
        );
        write_session(
            &dir,
            "b.jsonl",
            "{\"role\":\"user\",\"content\":\"игла три\"}\n",
        );
        let result = call(&dir, &json!({"query": "игла", "limit": 2})).unwrap();
        assert_eq!(result["matches"].as_array().unwrap().len(), 2);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn excerpt_carries_sixty_chars_of_context() {
        let dir = temp_dir("excerpt");
        let content = format!("{}needle{}", "a".repeat(100), "b".repeat(100));
        let line = format!("{{\"role\":\"assistant\",\"content\":\"{content}\"}}\n");
        write_session(&dir, "a.jsonl", &line);
        let result = call(&dir, &json!({"query": "NEEDLE"})).unwrap();
        let excerpt = result["matches"][0]["excerpt"].as_str().unwrap();
        assert_eq!(
            excerpt,
            format!("{}needle{}", "a".repeat(60), "b".repeat(60))
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn broken_jsonl_line_is_skipped() {
        let dir = temp_dir("broken");
        write_session(
            &dir,
            "a.jsonl",
            concat!(
                "{\"role\":\"user\",\"content\":\"первая метка\"}\n",
                "{битая строка без json\n",
                "{\"role\":\"assistant\",\"content\":\"вторая метка\"}\n",
            ),
        );
        let result = call(&dir, &json!({"query": "метка"})).unwrap();
        let matches = result["matches"].as_array().unwrap();
        assert_eq!(matches.len(), 2);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn cyrillic_query_matches_case_insensitive() {
        let dir = temp_dir("cyrillic");
        write_session(
            &dir,
            "a.jsonl",
            "{\"role\":\"user\",\"content\":\"Привет, МИР!\",\"ts\":\"2026-08-12T10:00:00Z\"}\n",
        );
        let result = call(&dir, &json!({"query": "привет, мир"})).unwrap();
        let matches = result["matches"].as_array().unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0]["excerpt"], "Привет, МИР!");
        assert_eq!(matches[0]["ts"], "2026-08-12T10:00:00Z");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_dir_is_empty_result_not_error() {
        let dir =
            std::env::temp_dir().join(format!("berimor-ses-test-missing-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let result = call(&dir, &json!({"query": "что угодно"})).unwrap();
        assert_eq!(result["matches"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn explicit_zero_limit_returns_no_matches() {
        let dir = temp_dir("zero-limit");
        write_session(
            &dir,
            "a.jsonl",
            "{\"role\":\"user\",\"content\":\"игла в стоге\"}\n",
        );
        let result = call(&dir, &json!({"query": "игла", "limit": 0})).unwrap();
        assert!(result["matches"].as_array().unwrap().is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn invalid_args_are_errors() {
        let dir = temp_dir("args");
        assert!(call(&dir, &json!({})).is_err());
        assert!(call(&dir, &json!({"query": ""})).is_err());
        assert!(call(&dir, &json!({"query": "x", "role": "system"})).is_err());
        assert!(call(&dir, &json!({"query": "x", "limit": "много"})).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }
}
