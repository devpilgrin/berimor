//! Инструмент `files.edit` — волна A1 спецификации
//! `docs/rnd/builtin-tools-waves-spec.md`: точечная замена в существующем
//! файле по строковому якорю (строгий подстрочный поиск, НЕ regex).
//!
//! Контракт: 0 вхождений якоря, неуникальный якорь без `replace_all`,
//! пустой `old_string`, несуществующий файл и файл больше
//! [`CONTENT_CAP`] — говорящие ошибки [`DispatchError`]; файл НЕ
//! создаётся и НЕ изменяется ни в одном из этих случаев. Успешный ответ:
//! `{path, replacements, bytes}` (bytes — новый размер файла).
//!
//! Безопасность — до сюда (capability-гейт, jail, mutates=true в
//! политиках родителя); модуль — исполнитель уже одобренной правки.
//! Регистрация в `builtin_dispatch` (BUILTIN_TOOLS/ветка call/политика)
//! — задача родителя, здесь только чистая функция.

use berimor_executors::tool_only::DispatchError;
use serde_json::{json, Value};
use std::io::Read;
use std::path::Path;

use crate::builtin_dispatch::{err_str, resolve_from, CONTENT_CAP};

/// Имя инструмента — фигурирует в DispatchError.tool.
const TOOL: &str = "files.edit";

/// Точка входа инструмента. `root` — канонизированный корень workspace
/// (относительные пути резолвятся от него, выход за корень уже отклонён
/// гейтом). `args`: `{path, old_string, new_string, replace_all?}`.
/// allow(dead_code) — до интеграции родителем (ветка в builtin_dispatch),
/// по образцу pub(crate)-хелперов resolve_from/err_str; убрать с первым
/// потребителем.
pub fn call(root: &Path, args: &Value) -> Result<Value, DispatchError> {
    let raw = args["path"]
        .as_str()
        .ok_or_else(|| err_str(TOOL, "аргумент 'path' обязателен (строка)"))?;
    let old = args["old_string"]
        .as_str()
        .ok_or_else(|| err_str(TOOL, "аргумент 'old_string' обязателен (строка)"))?;
    let new = args["new_string"]
        .as_str()
        .ok_or_else(|| err_str(TOOL, "аргумент 'new_string' обязателен (строка)"))?;
    let replace_all = args["replace_all"].as_bool().unwrap_or(false);
    // Пустой якорь совпал бы «везде» — это всегда ошибка вызвавшего,
    // а не просьба ничего не делать.
    if old.is_empty() {
        return Err(err_str(TOOL, "аргумент 'old_string' не может быть пустым"));
    }
    let path = resolve_from(root, raw);
    // Файл обязан существовать: files.edit — правка, не создание
    // (создание — территория files.write).
    let file = std::fs::File::open(&path).map_err(|e| {
        err_str(
            TOOL,
            format!("не удалось открыть '{}': {e}", path.display()),
        )
    })?;
    let mut buf = Vec::new();
    file.take(CONTENT_CAP + 1)
        .read_to_end(&mut buf)
        .map_err(|e| err_str(TOOL, format!("не удалось прочитать: {e}")))?;
    if buf.len() as u64 > CONTENT_CAP {
        return Err(err_str(
            TOOL,
            format!("файл '{}' больше капа {CONTENT_CAP} байт", path.display()),
        ));
    }
    // Строгий UTF-8, не lossy: замена и запись обратно не должна
    // молча перекодировать бинарное содержимое.
    let content = String::from_utf8(buf).map_err(|_| {
        err_str(
            TOOL,
            format!(
                "файл '{}' не UTF-8 — строковая замена невозможна",
                path.display()
            ),
        )
    })?;
    // Подстрочный подсчёт (не regex): matches — непересекающиеся
    // вхождения, та же семантика, что у str::replace/replacen.
    let occurrences = content.matches(old).count();
    if occurrences == 0 {
        return Err(err_str(TOOL, format!("якорь не найден в '{raw}'")));
    }
    if occurrences > 1 && !replace_all {
        return Err(err_str(
            TOOL,
            format!(
                "якорь не уникален ({occurrences} вхождений) — уточните 'old_string' \
                 или передайте 'replace_all': true"
            ),
        ));
    }
    // replacen/replace байтобезопасны для UTF-8 (границы совпадений —
    // границы подстроки old).
    let updated = if replace_all {
        content.replace(old, new)
    } else {
        content.replacen(old, new, 1)
    };
    std::fs::write(&path, &updated).map_err(|e| {
        err_str(
            TOOL,
            format!("не удалось записать '{}': {e}", path.display()),
        )
    })?;
    Ok(json!({
        "path": raw,
        "replacements": occurrences,
        "bytes": updated.len(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Temp-каталог по конвенции berimor-<mod>-test-<tag>-<pid>
    /// (tag различает тесты модуля — гонка temp-каталогов при
    /// параллельном прогоне).
    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("berimor-edit-test-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn single_replacement_reports_path_replacements_bytes() {
        let dir = temp_dir("single");
        std::fs::write(dir.join("a.txt"), "один два три").unwrap();
        let result = call(
            &dir,
            &json!({"path": "a.txt", "old_string": "два", "new_string": "ДВА"}),
        )
        .unwrap();
        assert_eq!(result["path"], "a.txt");
        assert_eq!(result["replacements"], 1);
        assert_eq!(result["bytes"], "один ДВА три".len() as u64);
        assert_eq!(
            std::fs::read_to_string(dir.join("a.txt")).unwrap(),
            "один ДВА три"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn replace_all_replaces_every_occurrence() {
        let dir = temp_dir("all");
        std::fs::write(dir.join("a.txt"), "x x x").unwrap();
        let result = call(
            &dir,
            &json!({
                "path": "a.txt",
                "old_string": "x",
                "new_string": "yy",
                "replace_all": true,
            }),
        )
        .unwrap();
        assert_eq!(result["replacements"], 3);
        assert_eq!(
            std::fs::read_to_string(dir.join("a.txt")).unwrap(),
            "yy yy yy"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn non_unique_anchor_without_replace_all_errors_and_keeps_file() {
        let dir = temp_dir("nonunique");
        std::fs::write(dir.join("a.txt"), "якорь и ещё якорь").unwrap();
        let err = call(
            &dir,
            &json!({"path": "a.txt", "old_string": "якорь", "new_string": "z"}),
        )
        .unwrap_err();
        assert!(err.reason.contains("не уникален"), "{}", err.reason);
        assert!(err.reason.contains("2"), "{}", err.reason);
        // Файл не тронут.
        assert_eq!(
            std::fs::read_to_string(dir.join("a.txt")).unwrap(),
            "якорь и ещё якорь"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_anchor_errors_and_keeps_file() {
        let dir = temp_dir("notfound");
        std::fs::write(dir.join("a.txt"), "содержимое").unwrap();
        let err = call(
            &dir,
            &json!({"path": "a.txt", "old_string": "нет-такого", "new_string": "z"}),
        )
        .unwrap_err();
        assert!(err.reason.contains("якорь не найден"), "{}", err.reason);
        assert_eq!(
            std::fs::read_to_string(dir.join("a.txt")).unwrap(),
            "содержимое"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn cyrillic_multiline_anchor_round_trip() {
        let dir = temp_dir("cyrillic");
        std::fs::write(
            dir.join("заметка.md"),
            "# Заголовок\n\nСтарая строка\nс продолжением\n",
        )
        .unwrap();
        let result = call(
            &dir,
            &json!({
                "path": "заметка.md",
                "old_string": "Старая строка\nс продолжением",
                "new_string": "Новая строка",
            }),
        )
        .unwrap();
        assert_eq!(result["replacements"], 1);
        assert_eq!(
            std::fs::read_to_string(dir.join("заметка.md")).unwrap(),
            "# Заголовок\n\nНовая строка\n"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn file_over_content_cap_errors() {
        let dir = temp_dir("cap");
        let big = "а".repeat(CONTENT_CAP as usize + 1);
        std::fs::write(dir.join("big.txt"), &big).unwrap();
        let err = call(
            &dir,
            &json!({"path": "big.txt", "old_string": "а", "new_string": "б"}),
        )
        .unwrap_err();
        assert!(err.reason.contains("больше капа"), "{}", err.reason);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_file_errors_and_is_not_created() {
        let dir = temp_dir("missing");
        let err = call(
            &dir,
            &json!({"path": "no-such.txt", "old_string": "a", "new_string": "b"}),
        )
        .unwrap_err();
        assert!(err.reason.contains("не удалось открыть"), "{}", err.reason);
        assert!(!dir.join("no-such.txt").exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn empty_old_string_errors() {
        let dir = temp_dir("empty");
        std::fs::write(dir.join("a.txt"), "текст").unwrap();
        let err = call(
            &dir,
            &json!({"path": "a.txt", "old_string": "", "new_string": "b"}),
        )
        .unwrap_err();
        assert!(
            err.reason.contains("не может быть пустым"),
            "{}",
            err.reason
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn golden_fixture_sample_edit() {
        // Фикстура без машинных путей: fixtures/golden/tools/files.edit/
        // относительно корня workspace (crate = crates/berimor-cli).
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/golden/tools/files.edit/sample.md");
        let dir = temp_dir("fixture");
        let target = dir.join("sample.md");
        std::fs::copy(&fixture, &target).unwrap();
        let result = call(
            &dir,
            &json!({
                "path": "sample.md",
                "old_string": "Статус: черновик",
                "new_string": "Статус: готово",
            }),
        )
        .unwrap();
        assert_eq!(result["replacements"], 1);
        let updated = std::fs::read_to_string(&target).unwrap();
        assert!(updated.contains("Статус: готово"));
        // Повторяющийся якорь фикстуры без replace_all — ошибка.
        let err = call(
            &dir,
            &json!({"path": "sample.md", "old_string": "якорь", "new_string": "z"}),
        )
        .unwrap_err();
        assert!(err.reason.contains("не уникален"), "{}", err.reason);
        std::fs::remove_dir_all(&dir).ok();
    }
}
