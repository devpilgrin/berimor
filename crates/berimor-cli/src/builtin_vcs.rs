//! Инструмент `vcs.git` — волна A3 спеки
//! `docs/rnd/builtin-tools-waves-spec.md`: чтение состояния
//! git-репозитория через СИСТЕМНЫЙ `git` (не libgit2) с фиксированными
//! наборами флагов на операцию.
//!
//! Граница доверия: произвольные флаги НЕ принимаются никогда
//! (deny-friendly) — аргументы `ref`/`path`, начинающиеся с `-`,
//! отклоняются, чтобы через них нельзя было протащить мутирующий флаг
//! (например `git diff --output=...`). Таймаут подпроцесса 15 секунд
//! (паттерн try_wait+kill, как в `terminal.exec`), вывод капается
//! [`TERMINAL_OUTPUT_CAP`] на поток. `mutates: false` — инструмент
//! только читает: status/diff/log/show не меняют ни рабочее дерево,
//! ни индекс, ни историю.

use berimor_executors::tool_only::DispatchError;
use serde_json::{json, Value};
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::builtin_dispatch::{err_str, resolve_from, TERMINAL_OUTPUT_CAP};

/// Имя инструмента — для DispatchError.
const TOOL: &str = "vcs.git";
/// Таймаут git-подпроцесса по спеке A3: без него зависший git
/// (например, на битом индексе) повесил бы синхронный движок.
const GIT_TIMEOUT: Duration = Duration::from_secs(15);
/// Лимит записей `log` по умолчанию и его потолок (спека A3).
const LOG_LIMIT_DEFAULT: u64 = 20;
const LOG_LIMIT_CAP: u64 = 100;

/// Точка входа: разбор args по контракту A3 и запуск git.
/// allow(dead_code) — до интеграции родителя ветки `vcs.git` в
/// `BuiltinToolDispatch::call` вызовов вне тестов нет (тот же приём,
/// что у pub(crate)-хелперов в builtin_dispatch).
pub fn call(root: &Path, args: &Value) -> Result<Value, DispatchError> {
    let op = args["op"]
        .as_str()
        .ok_or_else(|| err_str(TOOL, "аргумент 'op' обязателен (status|diff|log|show)"))?;
    let path = args["path"].as_str();
    // Ссылка (ref) проверяется ДО построения argv: значение с '-'
    // превратилось бы в произвольный флаг командной строки.
    let git_ref = args["ref"].as_str();
    if let Some(r) = git_ref {
        if r.starts_with('-') && r != "--cached" {
            return Err(err_str(
                TOOL,
                "произвольные флаги запрещены: 'ref' не должен начинаться с '-'",
            ));
        }
    }
    if let Some(p) = path {
        if p.starts_with('-') {
            return Err(err_str(
                TOOL,
                "произвольные флаги запрещены: 'path' не должен начинаться с '-'",
            ));
        }
    }

    // Фиксированные наборы флагов на операцию (спека A3). Путь
    // передаётся после `--` — отсечка трактует его строго как путь,
    // даже если он совпадает с именем ревизии.
    let mut argv: Vec<String> = Vec::new();
    match op {
        "status" => {
            argv.extend(["status".into(), "--short".into()]);
        }
        "diff" => {
            argv.push("diff".into());
            if let Some(r) = git_ref {
                argv.push(r.to_string());
            }
            if let Some(p) = path {
                argv.extend(["--".into(), resolve_from(root, p).display().to_string()]);
            }
        }
        "log" => {
            let limit = match args["limit"].as_u64() {
                Some(n) => n.min(LOG_LIMIT_CAP),
                None if args["limit"].is_null() => LOG_LIMIT_DEFAULT,
                None => {
                    return Err(err_str(TOOL, "аргумент 'limit' должен быть числом"));
                }
            };
            argv.extend([
                "log".into(),
                "--oneline".into(),
                "-n".into(),
                limit.to_string(),
            ]);
            if let Some(p) = path {
                argv.extend(["--".into(), resolve_from(root, p).display().to_string()]);
            }
        }
        "show" => {
            argv.push("show".into());
            // ref обязателен семантически, default HEAD (спека A3).
            argv.push(git_ref.unwrap_or("HEAD").to_string());
            if let Some(p) = path {
                argv.extend(["--".into(), resolve_from(root, p).display().to_string()]);
            }
        }
        _ => {
            return Err(err_str(
                TOOL,
                format!("неизвестная операция '{op}' (status|diff|log|show)"),
            ));
        }
    }

    run_git(root, &argv)
}

/// Запуск системного git в `root` с капом вывода и таймаутом 15 сек
/// (паттерн читателей с капом СРАЗУ + try_wait+kill из terminal.exec:
/// многословный git не съест память процесса до срабатывания таймаута).
fn run_git(root: &Path, argv: &[String]) -> Result<Value, DispatchError> {
    let mut child = Command::new("git")
        .args(argv)
        .current_dir(root)
        // Локаль принудительно C: распознавание «не репозиторий» идёт по
        // английскому тексту stderr, локализованный git ломал бы
        // диагностику (поймано тестом: русская сборка git).
        .env("LC_ALL", "C")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| err_str(TOOL, format!("не удалось запустить git: {e}")))?;
    let out_pipe = child.stdout.take().expect("stdout перенаправлен");
    let out_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = out_pipe.take(TERMINAL_OUTPUT_CAP + 1).read_to_end(&mut buf);
        buf
    });
    let err_pipe = child.stderr.take().expect("stderr перенаправлен");
    let err_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = err_pipe.take(TERMINAL_OUTPUT_CAP + 1).read_to_end(&mut buf);
        buf
    });
    let deadline = Instant::now() + GIT_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Ok(None) => {
                let _ = child.kill();
                break Err(err_str(
                    TOOL,
                    format!("таймаут {} сек", GIT_TIMEOUT.as_secs()),
                ));
            }
            Err(e) => break Err(err_str(TOOL, format!("wait: {e}"))),
        }
    };
    let _ = child.wait();
    // join с потолком (та же оговорка, что в terminal.exec): без него
    // осиротевший потомок держал бы трубу открытой до своего конца.
    let mut stdout = join_capped(out_reader);
    let stderr = join_capped(err_reader);
    let status = status?;
    let out_truncated = stdout.len() as u64 > TERMINAL_OUTPUT_CAP;
    stdout.truncate(TERMINAL_OUTPUT_CAP as usize);
    let stderr_text = String::from_utf8_lossy(&stderr);
    if !status.success() {
        // stderr git'а — в тексте ошибки (спека A3); не репозиторий —
        // говорящая ошибка с распознанным диагнозом.
        let code = status.code().unwrap_or(-1);
        if stderr_text.contains("not a git repository") {
            return Err(err_str(
                TOOL,
                format!("не git-репозиторий: {}", stderr_text.trim()),
            ));
        }
        return Err(err_str(
            TOOL,
            format!("git завершился с кодом {code}: {}", stderr_text.trim()),
        ));
    }
    Ok(json!({
        "stdout": String::from_utf8_lossy(&stdout),
        "truncated": out_truncated,
    }))
}

/// join с потолком 2 секунды (копия приватного хелпера из
/// builtin_dispatch — спека разрешает импортировать только
/// resolve_from/err_str/капы): вернуть буфер, если читатель
/// завершился, иначе пусто (поток отсоединяется и доигрывает сам).
fn join_capped(handle: std::thread::JoinHandle<Vec<u8>>) -> Vec<u8> {
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        if handle.is_finished() {
            return handle.join().unwrap_or_default();
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Temp-репозиторий по спеке: `git init -q -b main`, тег различает
    /// тесты (гонка temp-каталогов — известный камень).
    fn init_repo(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("berimor-vcs-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let out = Command::new("git")
            .args(["init", "-q", "-b", "main"])
            .current_dir(&dir)
            .output()
            .expect("git init запускается");
        assert!(
            out.status.success(),
            "git init: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        dir
    }

    /// Обычный temp-каталог БЕЗ репозитория — для негативного теста.
    fn plain_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("berimor-vcs-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Коммит с явными env-идентичностями — чистые машины без
    /// user.name/user.email в глобальном конфиге (спека A3).
    fn commit(dir: &Path, name: &str, content: &str, message: &str) {
        std::fs::write(dir.join(name), content).unwrap();
        git_ok(dir, &["add", name]);
        git_ok(dir, &["commit", "-q", "-m", message]);
    }

    fn git_ok(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "Berimor Test")
            .env("GIT_AUTHOR_EMAIL", "berimor-test@example.invalid")
            .env("GIT_COMMITTER_NAME", "Berimor Test")
            .env("GIT_COMMITTER_EMAIL", "berimor-test@example.invalid")
            .output()
            .expect("git запускается");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn cleanup(dir: &Path) {
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn status_empty_repo_is_empty_output() {
        let dir = init_repo("status-empty");
        let result = call(&dir, &json!({"op": "status"})).unwrap();
        assert_eq!(result["stdout"].as_str().unwrap().trim(), "");
        assert_eq!(result["truncated"], false);
        cleanup(&dir);
    }

    #[test]
    fn status_dirty_repo_lists_changes() {
        let dir = init_repo("status-dirty");
        commit(&dir, "a.txt", "раз\n", "первый");
        // Правка отслеживаемого файла + новый неотслеживаемый.
        std::fs::write(dir.join("a.txt"), "два\n").unwrap();
        std::fs::write(dir.join("b.txt"), "новый\n").unwrap();
        let result = call(&dir, &json!({"op": "status"})).unwrap();
        let stdout = result["stdout"].as_str().unwrap();
        assert!(stdout.contains(" M a.txt"), "stdout: {stdout}");
        assert!(stdout.contains("?? b.txt"), "stdout: {stdout}");
        cleanup(&dir);
    }

    #[test]
    fn log_after_two_commits_and_limit() {
        let dir = init_repo("log-two");
        commit(&dir, "a.txt", "раз\n", "первый коммит");
        commit(&dir, "b.txt", "два\n", "второй коммит");
        let result = call(&dir, &json!({"op": "log"})).unwrap();
        let stdout = result["stdout"].as_str().unwrap();
        assert_eq!(stdout.lines().count(), 2, "stdout: {stdout}");
        assert!(stdout.contains("первый коммит"), "stdout: {stdout}");
        assert!(stdout.contains("второй коммит"), "stdout: {stdout}");
        let one = call(&dir, &json!({"op": "log", "limit": 1})).unwrap();
        assert_eq!(one["stdout"].as_str().unwrap().lines().count(), 1);
        cleanup(&dir);
    }

    #[test]
    fn diff_sees_edit() {
        let dir = init_repo("diff-edit");
        commit(&dir, "a.txt", "старая строка\n", "первый");
        std::fs::write(dir.join("a.txt"), "старая строка\nновая строка\n").unwrap();
        let result = call(&dir, &json!({"op": "diff"})).unwrap();
        let stdout = result["stdout"].as_str().unwrap();
        assert!(stdout.contains("+новая строка"), "stdout: {stdout}");
        cleanup(&dir);
    }

    #[test]
    fn show_head_contains_commit_message() {
        let dir = init_repo("show-head");
        commit(&dir, "a.txt", "содержимое\n", "заголовок коммита");
        let result = call(&dir, &json!({"op": "show"})).unwrap();
        let stdout = result["stdout"].as_str().unwrap();
        assert!(stdout.contains("заголовок коммита"), "stdout: {stdout}");
        let explicit = call(&dir, &json!({"op": "show", "ref": "HEAD"})).unwrap();
        assert!(explicit["stdout"]
            .as_str()
            .unwrap()
            .contains("заголовок коммита"));
        cleanup(&dir);
    }

    #[test]
    fn not_a_repo_is_speaking_error() {
        let dir = plain_dir("not-repo");
        let result = call(&dir, &json!({"op": "status"}));
        let err = result.unwrap_err();
        assert!(
            err.reason.contains("не git-репозиторий"),
            "reason: {}",
            err.reason
        );
        assert_eq!(err.tool, TOOL);
        cleanup(&dir);
    }

    #[test]
    fn arbitrary_flags_are_rejected() {
        let dir = init_repo("flags-deny");
        let err = call(&dir, &json!({"op": "show", "ref": "--output=/tmp/pwn"})).unwrap_err();
        assert!(err.reason.contains("запрещены"), "reason: {}", err.reason);
        let err = call(&dir, &json!({"op": "diff", "path": "--cached"})).unwrap_err();
        assert!(err.reason.contains("запрещены"), "reason: {}", err.reason);
        let err = call(&dir, &json!({"op": "reset"})).unwrap_err();
        assert!(
            err.reason.contains("неизвестная операция"),
            "reason: {}",
            err.reason
        );
        cleanup(&dir);
    }
}
