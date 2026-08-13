//! Фоновый терминал: `terminal.start` / `terminal.output` / `terminal.kill`
//! — волна B, контракт B6 спецификации
//! `docs/rnd/builtin-tools-waves-spec.md`.
//!
//! В отличие от синхронного `terminal.exec` (общий таймаут 30 секунд),
//! фоновые процессы живут без потолка по времени: агент запускает
//! долгую команду (`start` → `{id}`), периодически читает вывод
//! (`output`) и при необходимости добивает процесс (`kill`).
//!
//! Та же оговорка про оболочку, что в `terminal.exec`: команда
//! выполняется через `sh -c`, а на части систем оболочка форкает
//! команду вместо exec — тогда `kill` бьёт саму оболочку, а её
//! осиротевший потомок может пережить её и доиграть до естественного
//! конца, держа трубу stdout/stderr открытой (потоки-читатели просто
//! завершатся по EOF трубы, процесс berimor это не блокирует).
//!
//! Буферы stdout/stderr кольцевые с капом [`TERMINAL_OUTPUT_CAP`] на
//! поток: при переполнении хранится ХВОСТ вывода и выставляется флаг
//! `truncated` (голова безвозвратно теряется — это сознательный
//! компромисс против съедания памяти командами вроде `yes`).
//!
//! `mutates`: start/kill — true (порождают/завершают процессы),
//! output — false (только чтение буферов и состояния).

// allow(dead_code) на весь модуль: до интеграции родителем (поле
// `bg` в BuiltinToolDispatch + ветки terminal.start/output/kill)
// потребителей вне тестов нет — тот же приём, что у builtin_todo /
// builtin_vcs; убрать с первым потребителем.
#![allow(dead_code)]

use std::collections::HashMap;
use std::io::Read;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use berimor_executors::tool_only::DispatchError;
use serde_json::{json, Value};

use crate::builtin_dispatch::{err_str, TERMINAL_OUTPUT_CAP};

/// Имена инструментов — для DispatchError.
const TOOL_START: &str = "terminal.start";
const TOOL_OUTPUT: &str = "terminal.output";
const TOOL_KILL: &str = "terminal.kill";

/// Кап кольцевого буфера в байтах (usize-форма TERMINAL_OUTPUT_CAP).
const RING_CAP: usize = TERMINAL_OUTPUT_CAP as usize;

/// Лимит живых записей реестра (ревью 2026-08-13 MEDIUM #5): без него
/// долгая сессия с частыми terminal.start раздувала память (завершённые
/// записи вечно держали до 2×RING_CAP буферов).
const MAX_PROCS: usize = 32;

/// Кольцевой буфер одного потока: при переполнении дропается голова,
/// остаётся хвост длиной не больше RING_CAP + флаг truncated.
#[derive(Default)]
struct StreamBuf {
    data: Vec<u8>,
    truncated: bool,
}

/// Добавить кусок вывода в кольцевой буфер с капом.
fn push_capped(buf: &mut StreamBuf, chunk: &[u8]) {
    if chunk.len() >= RING_CAP {
        // Кусок сам больше капа — храним только его хвост.
        buf.data.clear();
        buf.data.extend_from_slice(&chunk[chunk.len() - RING_CAP..]);
        buf.truncated = true;
        return;
    }
    buf.data.extend_from_slice(chunk);
    if buf.data.len() > RING_CAP {
        let excess = buf.data.len() - RING_CAP;
        buf.data.drain(..excess);
        buf.truncated = true;
    }
}

/// Живая запись реестра: дочерний процесс + разделяемые с
/// потоками-читателями кольцевые буферы + хэндлы читателей (ревью
/// MEDIUM #6: при «завершён» ждём их — иначе хвост усечён).
struct BgProc {
    child: Child,
    stdout: Arc<Mutex<StreamBuf>>,
    stderr: Arc<Mutex<StreamBuf>>,
    readers: Vec<std::thread::JoinHandle<()>>,
}

/// Потокобезопасный реестр фоновых процессов (контракт B6): поле
/// `bg` в `BuiltinToolDispatch` добавляет родитель, все вызовы — `&self`.
#[derive(Default)]
pub struct BgRegistry {
    procs: Arc<Mutex<HashMap<u64, BgProc>>>,
    counter: AtomicU64,
}

impl BgRegistry {
    /// Монотонный счётчик идентификаторов от 1 (1, 2, 3, ...).
    /// Публичный по спеке — родитель может опрашивать при клее.
    pub fn next_id(&self) -> u64 {
        self.counter.fetch_add(1, Ordering::Relaxed) + 1
    }

    /// Запустить оболочку с командой в каталоге `root`, stdout/stderr
    /// читаются потоками-читателями в кольцевые буферы. Ответ `{id}`.
    /// Оболочка — как у terminal.exec: sh -c / cmd /C (LOW #15 ревью).
    pub fn start(&self, root: &Path, command: &str) -> Result<Value, DispatchError> {
        // Кап реестра (MEDIUM #5): при давлении сначала вытесняем
        // завершённые записи; все живые — говорящая ошибка.
        {
            let mut procs = self.procs.lock().expect("мьютекс реестра не отравлен");
            if procs.len() >= MAX_PROCS {
                let finished: Vec<u64> = procs
                    .iter_mut()
                    .filter_map(|(id, p)| matches!(p.child.try_wait(), Ok(Some(_))).then_some(*id))
                    .collect();
                for id in finished {
                    procs.remove(&id);
                }
                if procs.len() >= MAX_PROCS {
                    return Err(err_str(
                        TOOL_START,
                        format!(
                            "лимит фоновых процессов ({MAX_PROCS}) — завершите лишние terminal.kill"
                        ),
                    ));
                }
            }
        }
        let (shell, flag) = if cfg!(windows) {
            ("cmd", "/C")
        } else {
            ("sh", "-c")
        };
        let mut child = Command::new(shell)
            .arg(flag)
            .arg(command)
            .current_dir(root)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| err_str(TOOL_START, format!("не удалось запустить sh: {e}")))?;

        let stdout = Arc::new(Mutex::new(StreamBuf::default()));
        let stderr = Arc::new(Mutex::new(StreamBuf::default()));
        let readers = vec![
            spawn_reader(child.stdout.take().expect("stdout перенаправлен"), &stdout),
            spawn_reader(child.stderr.take().expect("stderr перенаправлен"), &stderr),
        ];

        let id = self.next_id();
        self.procs
            .lock()
            .expect("мьютекс реестра не отравлен")
            .insert(
                id,
                BgProc {
                    child,
                    stdout,
                    stderr,
                    readers,
                },
            );
        Ok(json!({ "id": id }))
    }

    /// Срез вывода от байтового `offset` (по текущему содержимому
    /// кольцевого буфера): `{stdout, stderr, running, truncated}`.
    pub fn output(&self, id: u64, offset: usize) -> Result<Value, DispatchError> {
        let mut procs = self.procs.lock().expect("мьютекс реестра не отравлен");
        let proc = procs
            .get_mut(&id)
            .ok_or_else(|| err_str(TOOL_OUTPUT, format!("фоновый процесс #{id} не найден")))?;
        let running = match proc.child.try_wait() {
            Ok(Some(_)) => false,
            Ok(None) => true,
            Err(e) => return Err(err_str(TOOL_OUTPUT, format!("wait: {e}"))),
        };
        if !running {
            // MEDIUM #6: try_wait сказал «завершён», но читатели могут
            // ещё сливать остаток трубы — ждём их (≤1 с). Завершение
            // потока — точка синхронизации: после is_finished его
            // записи в буфер видны под мьютексом.
            for _ in 0..20 {
                if proc.readers.iter().all(|h| h.is_finished()) {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
        let (stdout, out_truncated) = {
            let buf = proc.stdout.lock().expect("мьютекс буфера не отравлен");
            (tail_from(&buf.data, offset), buf.truncated)
        };
        let (stderr, err_truncated) = {
            let buf = proc.stderr.lock().expect("мьютекс буфера не отравлен");
            (tail_from(&buf.data, offset), buf.truncated)
        };
        Ok(json!({
            "stdout": stdout,
            "stderr": stderr,
            "running": running,
            "truncated": out_truncated || err_truncated,
        }))
    }

    /// Послать SIGKILL ребёнку. `{killed: true}`, если процесс ещё
    /// работал и был сигнален; `{killed: false}`, если уже завершился
    /// сам. Несуществующий id — ошибка. Запись из реестра не
    /// удаляется: вывод остаётся доступным через `output`.
    pub fn kill(&self, id: u64) -> Result<Value, DispatchError> {
        let mut procs = self.procs.lock().expect("мьютекс реестра не отравлен");
        let proc = procs
            .get_mut(&id)
            .ok_or_else(|| err_str(TOOL_KILL, format!("фоновый процесс #{id} не найден")))?;
        let killed = match proc.child.try_wait() {
            Ok(Some(_)) => false,
            Ok(None) => {
                proc.child
                    .kill()
                    .map_err(|e| err_str(TOOL_KILL, format!("kill: {e}")))?;
                // wait сразу — иначе завершённый ребёнок висит зомби.
                let _ = proc.child.wait();
                true
            }
            Err(e) => return Err(err_str(TOOL_KILL, format!("wait: {e}"))),
        };
        Ok(json!({ "killed": killed }))
    }
}

/// Отсоединённый поток-читатель: гоняет куски трубы в кольцевой
/// буфер до EOF. Завершится сам, когда труба закроется (конец
/// процесса или закрытие дескрипторов при kill).
/// Читатель потока в кольцевой буфер до EOF/ошибки. Хэндл возвращается:
/// при «процесс завершён» вызывающий ждёт is_finished (ревью MEDIUM #6).
fn spawn_reader(
    mut pipe: impl Read + Send + 'static,
    buf: &Arc<Mutex<StreamBuf>>,
) -> std::thread::JoinHandle<()> {
    let buf = Arc::clone(buf);
    std::thread::spawn(move || {
        let mut chunk = [0u8; 8192];
        loop {
            match pipe.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let mut guard = buf.lock().expect("мьютекс буфера не отравлен");
                    push_capped(&mut guard, &chunk[..n]);
                }
            }
        }
    })
}

/// Строка из хвоста буфера от байтового offset (offset за пределами
/// текущего содержимого — пустая строка; битая граница UTF-8 —
/// lossy-замена).
fn tail_from(data: &[u8], offset: usize) -> String {
    if offset >= data.len() {
        return String::new();
    }
    String::from_utf8_lossy(&data[offset..]).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    /// Temp-каталог по спеке: тег различает тесты модуля (гонка
    /// temp-каталогов — известный камень).
    fn temp_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("berimor-tbg-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn cleanup(dir: &Path) {
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Дождаться завершения процесса (polling output), потолок 10 сек.
    fn wait_done(reg: &BgRegistry, id: u64) -> Value {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let out = reg.output(id, 0).unwrap();
            if !out["running"].as_bool().unwrap() {
                return out;
            }
            assert!(Instant::now() < deadline, "процесс #{id} не завершился");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn terminal_bg_echo_output_contains_text() {
        let dir = temp_dir("echo");
        let reg = BgRegistry::default();
        let started = reg.start(&dir, "printf 'привет-маркер\\n'").unwrap();
        let id = started["id"].as_u64().unwrap();
        assert_eq!(id, 1, "первый id — 1");
        let out = wait_done(&reg, id);
        assert!(
            out["stdout"].as_str().unwrap().contains("привет-маркер"),
            "stdout: {}",
            out["stdout"]
        );
        assert_eq!(out["truncated"], false);
        cleanup(&dir);
    }

    #[test]
    fn terminal_bg_sleep_kill_stops_running() {
        let dir = temp_dir("sleep-kill");
        let reg = BgRegistry::default();
        let id = reg.start(&dir, "sleep 30").unwrap()["id"].as_u64().unwrap();
        let out = reg.output(id, 0).unwrap();
        assert_eq!(out["running"], true, "sleep должен работать");
        let killed = reg.kill(id).unwrap();
        assert_eq!(killed["killed"], true);
        let out = reg.output(id, 0).unwrap();
        assert_eq!(out["running"], false, "после kill не работает");
        // Повторный kill завершённого — killed: false, не ошибка.
        assert_eq!(reg.kill(id).unwrap()["killed"], false);
        cleanup(&dir);
    }

    #[test]
    fn terminal_bg_output_with_offset() {
        let dir = temp_dir("offset");
        let reg = BgRegistry::default();
        let id = reg.start(&dir, "printf 'abcdef'").unwrap()["id"]
            .as_u64()
            .unwrap();
        wait_done(&reg, id);
        let out = reg.output(id, 2).unwrap();
        assert_eq!(out["stdout"].as_str().unwrap(), "cdef");
        // Offset за пределами содержимого — пустая строка.
        let out = reg.output(id, 1000).unwrap();
        assert_eq!(out["stdout"].as_str().unwrap(), "");
        cleanup(&dir);
    }

    #[test]
    fn terminal_bg_unknown_id_is_speaking_error() {
        let reg = BgRegistry::default();
        let err = reg.output(999, 0).unwrap_err();
        assert!(err.reason.contains("не найден"), "reason: {}", err.reason);
        assert_eq!(err.tool, TOOL_OUTPUT);
        let err = reg.kill(999).unwrap_err();
        assert!(err.reason.contains("не найден"), "reason: {}", err.reason);
        assert_eq!(err.tool, TOOL_KILL);
    }

    #[test]
    fn terminal_bg_buffer_cap_keeps_tail_and_truncated() {
        let dir = temp_dir("cap");
        let reg = BgRegistry::default();
        // Маленький цикл печати (seq, НЕ yes): 20000 строк по ~6 байт
        // — ~110 КиБ, заведомо больше капа 64 КиБ на поток.
        let id = reg.start(&dir, "seq 1 20000").unwrap()["id"]
            .as_u64()
            .unwrap();
        let out = wait_done(&reg, id);
        let stdout = out["stdout"].as_str().unwrap();
        assert_eq!(out["truncated"], true, "переполнение → truncated");
        assert!(
            stdout.len() <= RING_CAP,
            "буфер не больше капа: {}",
            stdout.len()
        );
        assert!(
            stdout.trim_end().ends_with("20000"),
            "хранится ХВОСТ: ...{}",
            &stdout[stdout.len().saturating_sub(40)..]
        );
        assert!(
            !stdout.contains("1\n2\n3\n"),
            "голова дропнута: {}...",
            &stdout[..20.min(stdout.len())]
        );
        cleanup(&dir);
    }
}
