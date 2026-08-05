//! Встроенные инструменты первого класса (ROADMAP §20.10) — реальные
//! действия без MCP и без `tool_stubs`: файлы, терминал, HTTP.
//!
//! Безопасность НЕ здесь, а до сюда: каждый вызов уже прошёл
//! capability-гейт (deny-статика по `PATH_KEYS`/`COMMAND_KEYS`, jail,
//! режимы подтверждений, политика mutates — S1/S2/S4). Этот модуль —
//! исполнитель уже одобренного действия; его собственные ограничения
//! (капы размеров, таймауты, запрет редиректов) — защита ресурсов, не
//! периметр. Расширение набора = новая ветка в `call` + политика в
//! [`builtin_policies`] + golden-кейс; имя инструмента — зарезервировано
//! (перекрыть встроенное имя заглушкой или MCP нельзя — порядок в
//! `CompositeToolDispatch`).

use berimor_capability::confirm::ToolPolicy;
use berimor_capability::net_gate;
use berimor_executors::tool_only::{DispatchError, ToolDispatch};
use serde_json::{json, Value};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Кап тела чтения/записи файла и тела HTTP-ответа — защита памяти
/// процесса (тот же принцип, что size-cap HTTP-провайдера, аудит 3.9).
const CONTENT_CAP: u64 = 1024 * 1024;
/// Кап числа записей листинга каталога.
const LIST_CAP: usize = 1000;
/// Кап stdout/stderr терминальной команды — на поток.
const TERMINAL_OUTPUT_CAP: u64 = 64 * 1024;
/// Таймаут терминальной команды: без него зависшая команда повесила бы
/// весь процесс (движок синхронный).
const TERMINAL_TIMEOUT: Duration = Duration::from_secs(30);
/// Таймаут HTTP-запроса.
const HTTP_TIMEOUT: Duration = Duration::from_secs(10);

/// Имена встроенных инструментов — зарезервированное пространство.
pub const BUILTIN_TOOLS: &[&str] = &[
    "files.read",
    "files.write",
    "files.list",
    "terminal.exec",
    "http.fetch",
    // Поручение субагенту (§20.17): само по себе не мутирует; действия
    // ребёнка проходят тот же гейт/подтверждения поштучно.
    "agents.run",
];

/// Политики capability-гейта для встроенных инструментов (S4): mutates
/// декларируется честно — `terminal.exec` всегда «изменяющий» (команда
/// может иметь побочные эффекты, deny-статика ловит опасные классы, но
/// не доказывает чистоту), HTTP — только GET, без тела, неизменяющий.
pub fn builtin_policies() -> Vec<(String, ToolPolicy)> {
    BUILTIN_TOOLS
        .iter()
        .map(|name| {
            let mutates = matches!(*name, "files.write" | "terminal.exec");
            (
                (*name).to_string(),
                ToolPolicy {
                    mutates: Some(mutates),
                    ..Default::default()
                },
            )
        })
        .collect()
}

/// Диспетчер встроенных инструментов. `workspace_root` — канонизированная
/// cwd запуска (тот же корень, что у jail в гейте): относительные пути
/// резолвятся от неё, физический выход за неё уже отклонён гейтом.
pub struct BuiltinToolDispatch {
    workspace_root: PathBuf,
    terminal_timeout: Duration,
}

impl BuiltinToolDispatch {
    pub fn new(workspace_root: PathBuf) -> Self {
        Self {
            workspace_root,
            terminal_timeout: TERMINAL_TIMEOUT,
        }
    }

    /// Таймаут терминала под контролем теста: постоянный 30-секундный
    /// прогон `sleep` в тестовом наборе — налог на каждый прогон.
    /// unix-only: единственный вызов — из cfg(unix)-теста, на Windows
    /// clippy видит мёртвый код (CI 2026-08-03).
    #[cfg(all(test, unix))]
    fn with_terminal_timeout(workspace_root: PathBuf, timeout: Duration) -> Self {
        Self {
            workspace_root,
            terminal_timeout: timeout,
        }
    }

    pub fn has_tool(tool: &str) -> bool {
        BUILTIN_TOOLS.contains(&tool)
    }

    fn resolve(&self, raw: &str) -> PathBuf {
        let path = Path::new(raw);
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.workspace_root.join(path)
        }
    }

    fn err(tool: &str, reason: impl Into<String>) -> DispatchError {
        DispatchError {
            tool: tool.into(),
            reason: reason.into(),
        }
    }

    fn read_string_capped<R: Read>(reader: R, cap: u64) -> Result<(String, bool), std::io::Error> {
        let mut buf = Vec::new();
        reader.take(cap + 1).read_to_end(&mut buf)?;
        let truncated = buf.len() as u64 > cap;
        buf.truncate(cap as usize);
        Ok((String::from_utf8_lossy(&buf).into_owned(), truncated))
    }
}

/// join с потолком 2 секунды: вернуть буфер, если читатель завершился,
/// иначе пусто (поток отсоединяется и доигрывает сам — см. комментарий
/// в terminal.exec про осиротевших потомков оболочки).
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

impl ToolDispatch for BuiltinToolDispatch {
    fn call(&self, tool: &str, args: &Value) -> Result<Value, DispatchError> {
        match tool {
            "files.read" => {
                let raw = args["path"]
                    .as_str()
                    .ok_or_else(|| Self::err(tool, "аргумент 'path' обязателен (строка)"))?;
                let path = self.resolve(raw);
                let file = std::fs::File::open(&path).map_err(|e| {
                    Self::err(
                        tool,
                        format!("не удалось открыть '{}': {e}", path.display()),
                    )
                })?;
                let (content, truncated) = Self::read_string_capped(file, CONTENT_CAP)
                    .map_err(|e| Self::err(tool, format!("не удалось прочитать: {e}")))?;
                Ok(json!({
                    "path": raw,
                    "content": content,
                    "truncated": truncated,
                }))
            }
            "files.write" => {
                let raw = args["path"]
                    .as_str()
                    .ok_or_else(|| Self::err(tool, "аргумент 'path' обязателен (строка)"))?;
                let content = args["content"]
                    .as_str()
                    .ok_or_else(|| Self::err(tool, "аргумент 'content' обязателен (строка)"))?;
                if content.len() as u64 > CONTENT_CAP {
                    return Err(Self::err(
                        tool,
                        format!("content превышает кап {CONTENT_CAP} байт"),
                    ));
                }
                let path = self.resolve(raw);
                // Родитель обязан существовать: молчаливое создание
                // директорий — неявный побочный эффект, не заказанный
                // действием (mutates касается файла, не структуры).
                std::fs::write(&path, content).map_err(|e| {
                    Self::err(
                        tool,
                        format!("не удалось записать '{}': {e}", path.display()),
                    )
                })?;
                Ok(json!({
                    "path": raw,
                    "bytes": content.len(),
                }))
            }
            "files.list" => {
                let raw = args["path"].as_str().unwrap_or(".");
                let path = self.resolve(raw);
                let mut entries: Vec<Value> = Vec::new();
                let read_dir = std::fs::read_dir(&path).map_err(|e| {
                    Self::err(
                        tool,
                        format!("не удалось открыть каталог '{}': {e}", path.display()),
                    )
                })?;
                for entry in read_dir {
                    if entries.len() >= LIST_CAP {
                        break;
                    }
                    let entry = entry.map_err(|e| Self::err(tool, format!("readdir: {e}")))?;
                    entries.push(json!({
                        "name": entry.file_name().to_string_lossy(),
                        "is_dir": entry.file_type().map(|t| t.is_dir()).unwrap_or(false),
                    }));
                }
                entries.sort_by_key(|e| e["name"].as_str().unwrap_or("").to_string());
                Ok(json!({
                    "path": raw,
                    "entries": entries,
                    "capped": entries.len() >= LIST_CAP,
                }))
            }
            "terminal.exec" => {
                let command = args["command"]
                    .as_str()
                    .ok_or_else(|| Self::err(tool, "аргумент 'command' обязателен (строка)"))?;
                // Оболочка платформы; разбор команды для гейта — по
                // POSIX-синтаксису (deny.rs), на Windows покрытие
                // cmd-специфики — честный пробел, задокументирован в
                // ROADMAP §20.10.
                let (shell, flag) = if cfg!(windows) {
                    ("cmd", "/C")
                } else {
                    ("sh", "-c")
                };
                let mut child = Command::new(shell)
                    .arg(flag)
                    .arg(command)
                    .current_dir(&self.workspace_root)
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn()
                    .map_err(|e| Self::err(tool, format!("не удалось запустить {shell}: {e}")))?;
                // Потоки читаются с капом СРАЗУ — `yes` не съест память
                // процесса до срабатывания таймаута.
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
                let deadline = Instant::now() + self.terminal_timeout;
                let status = loop {
                    match child.try_wait() {
                        Ok(Some(status)) => break Ok(status),
                        Ok(None) if Instant::now() < deadline => {
                            std::thread::sleep(Duration::from_millis(50));
                        }
                        Ok(None) => {
                            let _ = child.kill();
                            break Err(Self::err(
                                tool,
                                format!("таймаут {} сек", self.terminal_timeout.as_secs()),
                            ));
                        }
                        Err(e) => break Err(Self::err(tool, format!("wait: {e}"))),
                    }
                };
                let _ = child.wait();
                // join с потолком, не вечный: если оболочка НЕ exec'нула
                // команду (sh -c на части систем форкает), kill бьёт
                // оболочку, а осиротевший потомок держит трубу открытой
                // до своего естественного конца — read_to_end в потоке
                // ждёт его (CI ubuntu 2026-08-03: sleep 60 доиграл 61
                // секунду). Поток без join'а просто отсоединяется и
                // завершится сам, когда труба закроется.
                let mut stdout = join_capped(out_reader);
                let mut stderr = join_capped(err_reader);
                let status = status?;
                let out_truncated = stdout.len() as u64 > TERMINAL_OUTPUT_CAP;
                let err_truncated = stderr.len() as u64 > TERMINAL_OUTPUT_CAP;
                stdout.truncate(TERMINAL_OUTPUT_CAP as usize);
                stderr.truncate(TERMINAL_OUTPUT_CAP as usize);
                Ok(json!({
                    "exit_code": status.code().unwrap_or(-1),
                    "stdout": String::from_utf8_lossy(&stdout),
                    "stderr": String::from_utf8_lossy(&stderr),
                    "stdout_truncated": out_truncated,
                    "stderr_truncated": err_truncated,
                }))
            }
            "http.fetch" => {
                let raw = args["url"]
                    .as_str()
                    .ok_or_else(|| Self::err(tool, "аргумент 'url' обязателен (строка)"))?;
                let url = reqwest::Url::parse(raw)
                    .map_err(|e| Self::err(tool, format!("невалидный url: {e}")))?;
                let host = url
                    .host_str()
                    .ok_or_else(|| Self::err(tool, "url без хоста"))?;
                let port = url
                    .port_or_known_default()
                    .ok_or_else(|| Self::err(tool, "неизвестный порт (схема не http/https?)"))?;
                // Сетевой гейт (S3) — тот же, что у провайдеров моделей:
                // приватные/локальные адреса запрещены без исключений
                // (для провайдеров есть allow_private_endpoint, для
                // инструмента общего назначения — нет: его вызывает
                // декларация процесса, а не оператор напрямую).
                let decision = net_gate::check_host(host, port);
                if !decision.is_allowed() {
                    return Err(Self::err(
                        tool,
                        format!("сетевой гейт: {host}:{port} — адрес вне разрешённых сетей"),
                    ));
                }
                let client = reqwest::blocking::Client::builder()
                    .timeout(HTTP_TIMEOUT)
                    // Редиректы запрещены: цель редиректа не проходила бы
                    // гейт (обход одной проверкой).
                    .redirect(reqwest::redirect::Policy::none())
                    .build()
                    .map_err(|e| Self::err(tool, format!("http-клиент: {e}")))?;
                let response = client
                    .get(url)
                    .send()
                    .map_err(|e| Self::err(tool, format!("запрос: {e}")))?;
                let status = response.status().as_u16();
                let (body, truncated) = Self::read_string_capped(response, CONTENT_CAP)
                    .map_err(|e| Self::err(tool, format!("чтение тела: {e}")))?;
                Ok(json!({
                    "status": status,
                    "body": body,
                    "truncated": truncated,
                }))
            }
            _ => Err(Self::err(tool, "не встроенный инструмент")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dispatch() -> BuiltinToolDispatch {
        BuiltinToolDispatch::new(
            std::env::temp_dir()
                .canonicalize()
                .unwrap_or_else(|_| std::env::temp_dir()),
        )
    }

    #[test]
    fn files_write_then_read_round_trip() {
        let dir = std::env::temp_dir().join(format!("berimor-bt-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let dispatch = BuiltinToolDispatch::new(dir.canonicalize().unwrap());
        let write = dispatch
            .call(
                "files.write",
                &json!({"path": "note.txt", "content": "привет"}),
            )
            .unwrap();
        assert_eq!(write["bytes"], "привет".len() as u64);
        let read = dispatch
            .call("files.read", &json!({"path": "note.txt"}))
            .unwrap();
        assert_eq!(read["content"], "привет");
        assert_eq!(read["truncated"], false);
        let list = dispatch.call("files.list", &json!({"path": "."})).unwrap();
        assert!(list["entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["name"] == "note.txt"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn files_read_missing_file_is_dispatch_error_not_panic() {
        let result = dispatch().call("files.read", &json!({"path": "no-such-file.xyz"}));
        assert!(result.is_err());
    }

    #[cfg(unix)]
    #[test]
    fn terminal_exec_captures_output_and_exit_code() {
        let result = dispatch()
            .call(
                "terminal.exec",
                &json!({"command": "echo out; echo err >&2; exit 3"}),
            )
            .unwrap();
        assert_eq!(result["exit_code"], 3);
        assert!(result["stdout"].as_str().unwrap().contains("out"));
        assert!(result["stderr"].as_str().unwrap().contains("err"));
    }

    #[cfg(unix)]
    #[test]
    fn terminal_exec_timeout_kills_hanging_command() {
        let fast = BuiltinToolDispatch::with_terminal_timeout(
            std::env::temp_dir(),
            Duration::from_millis(500),
        );
        let start = Instant::now();
        let result = fast.call("terminal.exec", &json!({"command": "sleep 60"}));
        assert!(
            result.is_err(),
            "зависшая команда обязана упасть по таймауту"
        );
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "таймаут обязан сработать, не дожидаясь конца sleep"
        );
    }

    #[test]
    fn http_fetch_blocks_private_addresses_via_net_gate() {
        // Локальный сервер существует и отвечал бы — но гейт обязан
        // отклонить приватный адрес до всякого запроса.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let result = dispatch().call(
            "http.fetch",
            &json!({"url": format!("http://127.0.0.1:{port}/")}),
        );
        let err = result.unwrap_err();
        assert!(err.reason.contains("сетевой гейт"), "{}", err.reason);
    }

    #[test]
    fn http_fetch_rejects_unknown_scheme() {
        let result = dispatch().call("http.fetch", &json!({"url": "file:///etc/passwd"}));
        assert!(result.is_err());
    }

    #[test]
    fn unknown_tool_is_error() {
        assert!(dispatch().call("files.delete", &json!({})).is_err());
        assert!(!BuiltinToolDispatch::has_tool("files.delete"));
        assert!(BuiltinToolDispatch::has_tool("terminal.exec"));
    }

    #[test]
    fn builtin_policies_declare_mutates_honestly() {
        let policies = builtin_policies();
        let get = |name: &str| policies.iter().find(|(n, _)| n == name).unwrap().1.mutates;
        assert_eq!(get("files.read"), Some(false));
        assert_eq!(get("files.write"), Some(true));
        assert_eq!(get("terminal.exec"), Some(true));
        assert_eq!(get("http.fetch"), Some(false));
    }
}
