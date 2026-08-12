//! Инструмент `files.search` — поиск по дереву файлов рабочей области
//! (контракт A2 спецификации `docs/rnd/builtin-tools-waves-spec.md`).
//!
//! Два режима: `content` (regex по строкам с номерами и опциональным
//! контекстом) и `files` (globset по относительному пути). Обход —
//! walkdir от `resolve(path)`: скрытые каталоги, `.git` и `target`
//! пропускаются; файлы больше [`CONTENT_CAP`] не читаются и попадают в
//! `skipped` ответа. Пути в ответе и для glob-сопоставления —
//! относительные от корня рабочей области (`root`).
//!
//! Безопасность — как у остальных встроенных инструментов: вызов уже
//! прошёл capability-гейт, здесь только защита ресурсов (капы размера,
//! лимит совпадений). Инструмент ничего не изменяет (mutates: false).

use berimor_executors::tool_only::DispatchError;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

use crate::builtin_dispatch::{err_str, resolve_from, CONTENT_CAP};

/// Имя инструмента — для DispatchError и doc-ссылок.
const TOOL: &str = "files.search";
/// Лимит совпадений по умолчанию (контракт A2).
const DEFAULT_LIMIT: u64 = 100;
/// Потолок лимита совпадений — защита памяти ответа.
const MAX_LIMIT: u64 = 500;
/// Потолок строк контекста вокруг совпадения (контракт A2).
const MAX_CONTEXT: u64 = 5;

/// Точка входа инструмента (родитель регистрирует ветку в
/// `BuiltinToolDispatch::call` — spec, секция «Клей родителя»).
///
/// Args: `{pattern, mode?: "content"|"files" (default "content"),
/// path?: string (default "."), glob?: string, limit?: number
/// (default 100, cap 500), context?: number (default 0, cap 5)}`.
/// Ответ: `{matches: [...], truncated: bool, skipped: [{path, reason}]}`.
// allow(dead_code): до регистрации родителем потребителей вне тестов
// нет (тот же приём, что у хелперов builtin_dispatch) — убрать с
// первым потребителем.
pub fn call(root: &Path, args: &Value) -> Result<Value, DispatchError> {
    let pattern = args["pattern"]
        .as_str()
        .ok_or_else(|| err_str(TOOL, "аргумент 'pattern' обязателен (строка)"))?;
    let mode = args["mode"].as_str().unwrap_or("content");
    let raw_path = args["path"].as_str().unwrap_or(".");
    let limit = args["limit"]
        .as_u64()
        .unwrap_or(DEFAULT_LIMIT)
        .min(MAX_LIMIT) as usize;
    let context = args["context"].as_u64().unwrap_or(0).min(MAX_CONTEXT) as usize;

    let base = resolve_from(root, raw_path);
    if !base.is_dir() {
        return Err(err_str(
            TOOL,
            format!("каталог '{}' не найден", base.display()),
        ));
    }

    // Детерминизм ответа: обход сортируется — walkdir порядок зависит
    // от ФС, а golden-тесты и повторяемость движка требуют стабильности.
    let mut files: Vec<PathBuf> = Vec::new();
    for entry in WalkDir::new(&base)
        .into_iter()
        .filter_entry(|e| !is_skipped_dir(e))
    {
        let entry = entry.map_err(|e| err_str(TOOL, format!("обход каталога: {e}")))?;
        if entry.file_type().is_file() {
            files.push(entry.into_path());
        }
    }
    files.sort();

    let mut matches: Vec<Value> = Vec::new();
    let mut skipped: Vec<Value> = Vec::new();
    let mut truncated = false;

    match mode {
        "content" => {
            let re = regex::Regex::new(pattern)
                .map_err(|e| err_str(TOOL, format!("неверный regex '{pattern}': {e}")))?;
            // Необязательный glob-фильтр: какие файлы искать (относительный
            // путь от root), аналог grep --include.
            let include = match args["glob"].as_str() {
                Some(raw) => Some(build_glob(raw)?),
                None => None,
            };
            'walk: for file in &files {
                let rel = relative_display(root, file);
                if let Some(gs) = &include {
                    if !gs.is_match(Path::new(&rel)) {
                        continue;
                    }
                }
                let Some(content) = read_searchable(file, &rel, &mut skipped) else {
                    continue;
                };
                let lines: Vec<&str> = content.lines().collect();
                for (idx, line) in lines.iter().enumerate() {
                    if re.is_match(line) {
                        matches.push(json!({
                            "path": rel,
                            "line": idx + 1,
                            "text": line,
                            "context_lines": context_lines(&lines, idx, context),
                        }));
                        if matches.len() >= limit {
                            truncated = true;
                            break 'walk;
                        }
                    }
                }
            }
        }
        "files" => {
            let gs = build_glob(pattern)?;
            for file in &files {
                let rel = relative_display(root, file);
                if gs.is_match(Path::new(&rel)) {
                    matches.push(json!({ "path": rel }));
                    if matches.len() >= limit {
                        truncated = true;
                        break;
                    }
                }
            }
        }
        other => {
            return Err(err_str(
                TOOL,
                format!("неизвестный mode '{other}' (ожидается 'content' или 'files')"),
            ));
        }
    }

    Ok(json!({
        "matches": matches,
        "truncated": truncated,
        "skipped": skipped,
    }))
}

/// Скрытые каталоги (`.git` в том числе) и `target` не обходятся:
/// служебные данные VCS и сборочные артефакты — шум для поиска.
fn is_skipped_dir(entry: &walkdir::DirEntry) -> bool {
    if entry.depth() == 0 || !entry.file_type().is_dir() {
        return false;
    }
    let name = entry.file_name().to_string_lossy();
    name.starts_with('.') || name == "target"
}

/// Путь относительно корня рабочей области — единый вид для ответа и
/// glob-сопоставления. Вне корня (абсолютный `path`) — полный путь.
fn relative_display(root: &Path, file: &Path) -> String {
    file.strip_prefix(root)
        .unwrap_or(file)
        .to_string_lossy()
        .into_owned()
}

/// Компиляция glob-шаблона; битый шаблон — говорящая ошибка, как у regex.
fn build_glob(raw: &str) -> Result<globset::GlobSet, DispatchError> {
    globset::GlobBuilder::new(raw)
        .literal_separator(false)
        .build()
        .map(|g| {
            let mut set = globset::GlobSetBuilder::new();
            set.add(g);
            set.build()
        })
        .and_then(|r| r)
        .map_err(|e| err_str(TOOL, format!("неверный glob '{raw}': {e}")))
}

/// Прочитать файл для поиска: больше [`CONTENT_CAP`] — не читаем,
/// помечаем в `skipped` (контракт A2); нечитаемые — тоже skipped, поиск
/// остального дерева не роняем.
fn read_searchable(file: &Path, rel: &str, skipped: &mut Vec<Value>) -> Option<String> {
    let size = match std::fs::metadata(file) {
        Ok(meta) => meta.len(),
        Err(e) => {
            skipped.push(json!({"path": rel, "reason": format!("метаданные: {e}")}));
            return None;
        }
    };
    if size > CONTENT_CAP {
        skipped.push(json!({
            "path": rel,
            "reason": format!("файл больше капа {CONTENT_CAP} байт"),
        }));
        return None;
    }
    match std::fs::read(file) {
        // Lossy: бинарные/битые кодировки не роняют поиск — совпадения
        // по уцелевшим строкам лучше, чем отказ всего запроса.
        Ok(bytes) => Some(String::from_utf8_lossy(&bytes).into_owned()),
        Err(e) => {
            skipped.push(json!({"path": rel, "reason": format!("чтение: {e}")}));
            None
        }
    }
}

/// N строк контекста до и после совпадения (сама строка совпадения —
/// в полях `line`/`text`, сюда не дублируется).
fn context_lines(lines: &[&str], idx: usize, context: usize) -> Vec<Value> {
    if context == 0 {
        return Vec::new();
    }
    let from = idx.saturating_sub(context);
    let to = (idx + context + 1).min(lines.len());
    (from..to)
        .filter(|&i| i != idx)
        .map(|i| json!({"line": i + 1, "text": lines[i]}))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Фикстурное дерево спеки: `<workspace>/fixtures/golden/tools/files.search/tree`.
    fn fixture_tree() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/golden/tools/files.search/tree")
            .canonicalize()
            .expect("фикстурное дерево files.search обязано существовать")
    }

    /// Temp-каталог теста (tag различает тесты модуля — гонка
    /// temp-каталогов, см. «Общие правила» спеки).
    fn temp_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("berimor-search-test-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn paths(result: &Value) -> Vec<String> {
        result["matches"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["path"].as_str().unwrap().to_string())
            .collect()
    }

    #[test]
    fn content_match_reports_line_number_and_text() {
        let root = fixture_tree();
        let result = call(&root, &json!({"pattern": "^fn main"})).unwrap();
        assert_eq!(result["truncated"], false);
        let matches = result["matches"].as_array().unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0]["path"], "src/main.rs");
        assert_eq!(matches[0]["line"], 1);
        assert!(matches[0]["text"].as_str().unwrap().contains("fn main"));
    }

    #[test]
    fn content_match_cyrillic_text_and_filename() {
        let root = fixture_tree();
        let result = call(&root, &json!({"pattern": "привет"})).unwrap();
        let matches = result["matches"].as_array().unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0]["path"], "docs/привет.txt");
        assert_eq!(matches[0]["line"], 2);
        assert!(matches[0]["text"].as_str().unwrap().contains("мир"));
    }

    #[test]
    fn files_mode_matches_glob_by_relative_path() {
        let root = fixture_tree();
        let result = call(&root, &json!({"pattern": "**/*.rs", "mode": "files"})).unwrap();
        let mut found = paths(&result);
        found.sort();
        assert_eq!(found, vec!["src/lib.rs", "src/main.rs"]);
    }

    #[test]
    fn limit_caps_matches_and_marks_truncated() {
        let root = fixture_tree();
        // "маркер" встречается в README.md, src/lib.rs, src/main.rs.
        let result = call(&root, &json!({"pattern": "маркер", "limit": 1})).unwrap();
        assert_eq!(result["matches"].as_array().unwrap().len(), 1);
        assert_eq!(result["truncated"], true);
        // Без упора в лимит — все три совпадения, флага нет.
        let full = call(&root, &json!({"pattern": "маркер"})).unwrap();
        assert_eq!(full["matches"].as_array().unwrap().len(), 3);
        assert_eq!(full["truncated"], false);
    }

    #[test]
    fn hidden_dirs_git_and_target_are_skipped() {
        let root = fixture_tree();
        // Маркер лежит и в .git/config, и в .hidden/, и в target/ —
        // ни один из этих путей не должен попасть в ответ.
        let result = call(&root, &json!({"pattern": "маркер"})).unwrap();
        for path in paths(&result) {
            assert!(!path.starts_with(".git"), "протёк .git: {path}");
            assert!(
                !path.starts_with(".hidden"),
                "протёк скрытый каталог: {path}"
            );
            assert!(!path.starts_with("target"), "протёк target: {path}");
        }
    }

    #[test]
    fn broken_regex_is_talking_error() {
        let root = fixture_tree();
        let err = call(&root, &json!({"pattern": "(незакрытая"})).unwrap_err();
        assert_eq!(err.tool, TOOL);
        assert!(
            err.reason.contains("неверный regex"),
            "ожидалась говорящая ошибка: {}",
            err.reason
        );
    }

    #[test]
    fn context_lines_surround_the_match() {
        let root = fixture_tree();
        let result = call(&root, &json!({"pattern": "привет", "context": 1})).unwrap();
        let ctx = result["matches"][0]["context_lines"].as_array().unwrap();
        assert_eq!(ctx.len(), 2);
        assert_eq!(ctx[0]["line"], 1);
        assert!(ctx[0]["text"].as_str().unwrap().contains("первая"));
        assert_eq!(ctx[1]["line"], 3);
        assert!(ctx[1]["text"].as_str().unwrap().contains("третья"));
        // Без запрошенного контекста поле пустое.
        let plain = call(&root, &json!({"pattern": "привет"})).unwrap();
        assert_eq!(
            plain["matches"][0]["context_lines"]
                .as_array()
                .unwrap()
                .len(),
            0
        );
    }

    #[test]
    fn content_mode_glob_filters_searched_files() {
        let root = fixture_tree();
        let result = call(&root, &json!({"pattern": "маркер", "glob": "*.md"})).unwrap();
        assert_eq!(paths(&result), vec!["README.md".to_string()]);
    }

    #[test]
    fn oversized_file_goes_to_skipped_not_matched() {
        let dir = temp_dir("cap");
        let big = dir.join("big.txt");
        // Кап + запас: маркер внутри, но файл читать нельзя.
        let mut body = "x".repeat(CONTENT_CAP as usize + 16);
        body.push_str("маркер");
        std::fs::write(&big, body).unwrap();
        std::fs::write(dir.join("small.txt"), "маркер в маленьком\n").unwrap();

        let result = call(&dir, &json!({"pattern": "маркер"})).unwrap();
        assert_eq!(paths(&result), vec!["small.txt".to_string()]);
        let skipped = result["skipped"].as_array().unwrap();
        assert_eq!(skipped.len(), 1);
        assert_eq!(skipped[0]["path"], "big.txt");
        assert!(skipped[0]["reason"]
            .as_str()
            .unwrap()
            .contains("больше капа"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn unknown_mode_and_missing_pattern_are_errors() {
        let root = fixture_tree();
        let err = call(&root, &json!({"pattern": "x", "mode": "по-своему"})).unwrap_err();
        assert!(err.reason.contains("неизвестный mode"));
        let err = call(&root, &json!({})).unwrap_err();
        assert!(err.reason.contains("'pattern' обязателен"));
    }
}
