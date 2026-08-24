//! `berimor serve` (prompt-next-wave.md задача 2) — минимальный HTTP-
//! сервис поверх уже существующих операций CLI: запуск процесса по
//! имени файла, статус расписаний (чтение `ScheduleStore`), список
//! живых сессий (чтение реестра `sessions.rs`).
//!
//! Реализация — `std::net::TcpListener`, поток на соединение, ручной
//! разбор HTTP/1.1 (метод/путь/заголовки/`Content-Length`+тело) — тот же
//! приём, что уже используют мок-серверы e2e-тестов CLI
//! (`chat_cli.rs`/`entity_graph_cli.rs`/`memory_write_cli.rs`), без
//! новой зависимости (ни tokio с фичей `net`, ни HTTP-фреймворка):
//! нагрузка — локальный dev/ops-инструмент, не публичный сервис, где
//! оправдан асинхронный стек.
//!
//! Аутентификация ОБЯЗАТЕЛЬНА (I2, `security-model.md` §3: исполнение
//! процессов по сети не бывает анонимным) — токен читается из
//! переменной окружения, ИМЯ которой задаёт `[serve] token_env`
//! (значение никогда не хранится в файле конфигурации, тот же принцип,
//! что `ProviderConfig::api_key_env`). Отсутствие `token_env` — отказ
//! стартовать, не сервис без охраны. Порт слушает ТОЛЬКО `127.0.0.1`
//! (loopback) — не настраиваемый адрес привязки: раскрытие вовне —
//! осознанное решение оператора через собственный reverse-proxy/туннель,
//! не флаг этого сервиса.
//!
//! `POST /run` исполняет процесс СИНХРОННО (тот же путь, что `berimor
//! run`/`daemon.rs` — блокирует ответ до `Finished`/ошибки): очередь
//! фоновых задач и статус-поллинг — вне объёма этой задачи.
//!
//! **Найдено при написании e2e-теста, не гипотетически**: у `berimor
//! serve` нет терминала, поэтому ЛЮБОЕ решение, которое обычно
//! спрашивает человека через stdin, получает неинтерактивный отказ —
//! это не только `human_gate` (`ask_line` на EOF → false → отказ), но и
//! РЕЖИМ ПОДТВЕРЖДЕНИЙ `smart`/`manual` (S4) для мутирующих
//! `tool`-действий: `TerminalConfirmer::confirm` печатает вопрос в
//! stderr и читает тот же stdin — на EOF тоже `false` → действие
//! отклоняется, весь шаг падает ошибкой. Оператор `berimor serve`
//! ОБЯЗАН настроить `confirmation_mode = "off"` (deny-статика и jail
//! остаются реальной линией защиты, режим `off` снимает только ВОПРОС,
//! не проверку — `security-model.md` §3) и/или конкретные инструменты в
//! `auto_confirm` — иначе любой процесс с мутирующим шагом гарантированно
//! упадёт на первом же таком шаге. То же ограничение по духу, что уже
//! есть у `daemon.rs`'s срабатываний по расписанию (та же
//! `TerminalConfirmer`, тот же незанятый stdin) — не новая проблема этой
//! задачи, но здесь впервые явно задокументирована и покрыта тестом.

use crate::config::Config;
use berimor_storage::{EventLog, ScheduleStore, SqliteEventLog};
use berimor_types::event::ProcessInstanceId;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};

#[derive(Debug, thiserror::Error)]
pub enum ServeError {
    #[error("[serve] token_env не задан в конфиге — сервис не запускается без аутентификации")]
    NoTokenEnv,
    #[error("переменная окружения {0} не установлена или пуста — задайте токен перед запуском")]
    EmptyToken(String),
    #[error("не удалось привязать 127.0.0.1:{0}: {1}")]
    Bind(u16, String),
}

/// `berimor serve [--port N]`. Блокирует поток вызова (`accept`-цикл) —
/// возвращается только на ошибке привязки порта.
pub fn run(config: &Config, port_override: Option<u16>) -> Result<(), ServeError> {
    let (listener, token) = bind(config, port_override)?;
    let port = listener.local_addr().map(|a| a.port()).unwrap_or(0);
    eprintln!("[berimor] serve: слушаю 127.0.0.1:{port} (только loopback)");
    serve_loop(listener, config, &token);
    Ok(())
}

/// Проверка токена + привязка порта — отдельно от бесконечного цикла,
/// чтобы тесты могли узнать реальный порт (`port_override: Some(0)` —
/// ОС выбирает свободный) до того, как поток заблокируется в `accept`.
fn bind(config: &Config, port_override: Option<u16>) -> Result<(TcpListener, String), ServeError> {
    let token_env = config
        .serve
        .token_env
        .as_ref()
        .ok_or(ServeError::NoTokenEnv)?;
    let token = std::env::var(token_env)
        .ok()
        .filter(|v| !v.is_empty())
        .ok_or_else(|| ServeError::EmptyToken(token_env.clone()))?;
    let port = port_override.unwrap_or(config.serve.port);
    let listener = TcpListener::bind(("127.0.0.1", port))
        .map_err(|err| ServeError::Bind(port, err.to_string()))?;
    Ok((listener, token))
}

fn serve_loop(listener: TcpListener, config: &Config, token: &str) {
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let config = config.clone();
        let token = token.to_string();
        std::thread::spawn(move || handle_connection(stream, &config, &token));
    }
}

pub(crate) struct Request {
    pub(crate) method: String,
    pub(crate) path: String,
    pub(crate) authorization: Option<String>,
    pub(crate) x_github_event: Option<String>,
    pub(crate) x_hub_signature: Option<String>,
    pub(crate) body: Vec<u8>,
}

/// Разбор запроса. `Err` — соединение нечитаемо (клиент оборвал связь и
/// т.п.) — обрабатывающий код просто закрывает поток, не паникует.
fn read_request(stream: &TcpStream) -> Result<Request, String> {
    let mut reader = BufReader::new(stream.try_clone().map_err(|e| e.to_string())?);
    let mut request_line = String::new();
    reader
        .read_line(&mut request_line)
        .map_err(|e| e.to_string())?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let path = parts.next().unwrap_or("").to_string();

    let mut content_length = 0usize;
    let mut authorization: Option<String> = None;
    let mut x_github_event: Option<String> = None;
    let mut x_hub_signature: Option<String> = None;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).map_err(|e| e.to_string())?;
        if line.trim().is_empty() {
            break;
        }
        if let Some((key, value)) = line.split_once(':') {
            let key = key.trim();
            let value = value.trim().to_string();
            if key.eq_ignore_ascii_case("content-length") {
                content_length = value.parse().unwrap_or(0);
            } else if key.eq_ignore_ascii_case("authorization") {
                authorization = Some(value);
            } else if key.eq_ignore_ascii_case("x-github-event") {
                x_github_event = Some(value);
            } else if key.eq_ignore_ascii_case("x-hub-signature-256") {
                x_hub_signature = Some(value);
            }
        }
    }
    let mut body = vec![0u8; content_length];
    reader.read_exact(&mut body).map_err(|e| e.to_string())?;

    Ok(Request {
        method,
        path,
        authorization,
        x_github_event,
        x_hub_signature,
        body,
    })
}

pub(crate) fn write_json(stream: &mut TcpStream, status: u16, body: &Value) {
    let text = body.to_string();
    let status_line = match status {
        200 => "200 OK",
        400 => "400 Bad Request",
        401 => "401 Unauthorized",
        404 => "404 Not Found",
        _ => "500 Internal Server Error",
    };
    let response = format!(
        "HTTP/1.1 {status_line}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{text}",
        text.len()
    );
    let _ = stream.write_all(response.as_bytes());
}

fn handle_connection(mut stream: TcpStream, config: &Config, token: &str) {
    let request = match read_request(&stream) {
        Ok(r) => r,
        Err(_) => return,
    };
    // Вебхуки GitHub — ДО bearer: у GitHub нет нашего токена, его
    // аутентификация — HMAC-подпись тела (волна F, ghapp.rs).
    if request.method == "POST" && request.path == "/webhooks/github" {
        return crate::ghapp_serve::handle_github_webhook(&mut stream, config, &request);
    }
    // Аутентификация — ДО любой другой обработки, одинаково для всех
    // маршрутов (I2: нет исключения вроде «/health без токена» — меньше
    // мест, которые можно забыть защитить). Сравнение заголовка целиком
    // с ожидаемым значением — не частичное совпадение.
    let expected = format!("Bearer {token}");
    if request.authorization.as_deref() != Some(expected.as_str()) {
        write_json(&mut stream, 401, &json!({"error": "unauthorized"}));
        return;
    }

    match (request.method.as_str(), request.path.as_str()) {
        ("GET", "/sessions") => handle_sessions(&mut stream, config),
        ("GET", "/schedules") => handle_schedules(&mut stream, config),
        ("POST", "/run") => handle_run(&mut stream, config, &request.body),
        _ => write_json(&mut stream, 404, &json!({"error": "not found"})),
    }
}

fn open_storage(config: &Config) -> Result<SqliteEventLog, String> {
    SqliteEventLog::open(&config.storage_path).map_err(|err| err.to_string())
}

/// `GET /sessions` — живые сессии хоста, то же чтение, что `sessions
/// cmd_sessions`, но JSON вместо печати в stdout.
fn handle_sessions(stream: &mut TcpStream, config: &Config) {
    let storage = match open_storage(config) {
        Ok(s) => s,
        Err(err) => return write_json(stream, 500, &json!({"error": err})),
    };
    let events = storage
        .replay(&ProcessInstanceId(
            crate::sessions::SESSIONS_INSTANCE_ID.to_string(),
        ))
        .unwrap_or_default();
    let sessions = crate::sessions::fold_sessions(&events);
    let live: Vec<_> = sessions
        .iter()
        .filter(|s| !s.closed && s.pid_alive)
        .collect();
    write_json(stream, 200, &json!({"sessions": live}));
}

/// `GET /schedules` — все расписания по ближайшему срабатыванию, то же
/// чтение, что `daemon::schedule_list`, но JSON.
fn handle_schedules(stream: &mut TcpStream, config: &Config) {
    let storage = match open_storage(config) {
        Ok(s) => s,
        Err(err) => return write_json(stream, 500, &json!({"error": err})),
    };
    match storage.list_schedules() {
        Ok(schedules) => {
            let items: Vec<Value> = schedules
                .iter()
                .map(|s| {
                    json!({
                        "id": s.id.0,
                        "next_fire_ms": s.next_fire_ms,
                        "interval_ms": s.interval_ms,
                        "payload": s.payload,
                    })
                })
                .collect();
            write_json(stream, 200, &json!({"schedules": items}));
        }
        Err(err) => write_json(stream, 500, &json!({"error": err.to_string()})),
    }
}

/// `POST /run` — тело `{"process": "<путь>", "input": {...}}`. Исполняет
/// СИНХРОННО тем же путём, что `berimor run` (см. doc-комментарий
/// модуля про human_gate и отсутствие очереди).
fn handle_run(stream: &mut TcpStream, config: &Config, body: &[u8]) {
    let payload: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(err) => {
            return write_json(
                stream,
                400,
                &json!({"error": format!("тело запроса — не валидный JSON: {err}")}),
            )
        }
    };
    let Some(process) = payload.get("process").and_then(Value::as_str) else {
        return write_json(
            stream,
            400,
            &json!({"error": "поле 'process' (путь к YAML) обязательно"}),
        );
    };
    let input = payload.get("input").map(|v| v.to_string());

    // HTTP-сервис — по определению без терминала: подтверждение =
    // отказ с диагностикой (BR-05, тот же случай, что демон).
    match crate::run::run(config, process, &None, &input, true) {
        Ok(()) => write_json(stream, 200, &json!({"status": "finished"})),
        Err(crate::run::RunError::HumanDeclined) => write_json(
            stream,
            200,
            &json!({
                "status": "declined",
                "note": "human_gate отклонён неинтерактивно (нет терминала у berimor serve)"
            }),
        ),
        Err(err) => write_json(
            stream,
            500,
            &json!({"status": "error", "error": err.to_string()}),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ServeConfig;
    use std::net::{Shutdown, TcpStream};
    use std::sync::atomic::{AtomicU32, Ordering};

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    /// Уникальная переменная окружения на тест — `std::env::set_var`
    /// глобален процессу, тесты этого модуля выполняются в одном
    /// бинарнике параллельно (стандартный раннер `cargo test`).
    fn temp_config_with_token(token_value: &str) -> Config {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir =
            std::env::temp_dir().join(format!("berimor-serve-test-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let var_name = format!("BERIMOR_TEST_SERVE_TOKEN_{}_{n}", std::process::id());
        // Safety: тест-only, значение — литерал без управляющих символов,
        // имя переменной уникально на процесс+тест (см. комментарий выше).
        unsafe {
            std::env::set_var(&var_name, token_value);
        }
        Config {
            storage_path: dir.join("serve.db"),
            serve: ServeConfig {
                port: 0,
                token_env: Some(var_name),
            },
            ..Config::default()
        }
    }

    fn start_test_server(config: &Config) -> u16 {
        let (listener, token) = bind(config, Some(0)).expect("bind");
        let port = listener.local_addr().unwrap().port();
        let config = config.clone();
        std::thread::spawn(move || serve_loop(listener, &config, &token));
        port
    }

    fn raw_request(port: u16, request: &str) -> String {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream.write_all(request.as_bytes()).unwrap();
        stream.shutdown(Shutdown::Write).ok();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        response
    }

    fn get(port: u16, path: &str, token: &str) -> String {
        raw_request(
            port,
            &format!(
                "GET {path} HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer {token}\r\nConnection: close\r\n\r\n"
            ),
        )
    }

    #[test]
    fn bind_refuses_when_token_env_not_configured_in_config() {
        let config = Config {
            storage_path: std::env::temp_dir().join("berimor-serve-notoken.db"),
            ..Config::default()
        };
        let result = bind(&config, Some(0));
        assert!(matches!(result, Err(ServeError::NoTokenEnv)), "{result:?}");
    }

    #[test]
    fn bind_refuses_when_token_env_var_is_unset() {
        let config = Config {
            storage_path: std::env::temp_dir().join("berimor-serve-unsetvar.db"),
            serve: ServeConfig {
                port: 0,
                token_env: Some("BERIMOR_TEST_DEFINITELY_UNSET_VAR_XYZ_QWERTY".into()),
            },
            ..Config::default()
        };
        let result = bind(&config, Some(0));
        assert!(
            matches!(result, Err(ServeError::EmptyToken(_))),
            "{result:?}"
        );
    }

    #[test]
    fn request_without_authorization_header_is_401() {
        let config = temp_config_with_token("secret-1");
        let port = start_test_server(&config);
        let response = raw_request(
            port,
            "GET /sessions HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
        );
        assert!(response.starts_with("HTTP/1.1 401"), "{response}");
    }

    #[test]
    fn request_with_wrong_token_is_401() {
        let config = temp_config_with_token("secret-2");
        let port = start_test_server(&config);
        let response = get(port, "/sessions", "wrong-token");
        assert!(response.starts_with("HTTP/1.1 401"), "{response}");
    }

    #[test]
    fn request_with_correct_token_reaches_sessions_endpoint() {
        let config = temp_config_with_token("secret-3");
        let port = start_test_server(&config);
        let response = get(port, "/sessions", "secret-3");
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        assert!(response.contains("\"sessions\""), "{response}");
    }

    #[test]
    fn schedules_endpoint_returns_seeded_schedule() {
        let config = temp_config_with_token("secret-4");
        let storage = SqliteEventLog::open(&config.storage_path).unwrap();
        storage
            .upsert_schedule(&berimor_storage::Schedule {
                id: berimor_storage::ScheduleId("sched-test-1".into()),
                next_fire_ms: 9_999_999_999,
                interval_ms: None,
                payload: json!({"process_path": "p.yaml", "input": {}}),
            })
            .unwrap();
        let port = start_test_server(&config);
        let response = get(port, "/schedules", "secret-4");
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        assert!(response.contains("sched-test-1"), "{response}");
    }

    #[test]
    fn unknown_route_is_404() {
        let config = temp_config_with_token("secret-5");
        let port = start_test_server(&config);
        let response = get(port, "/nope", "secret-5");
        assert!(response.starts_with("HTTP/1.1 404"), "{response}");
    }

    /// Процесс без единого мутирующего шага (`branch`-подобный no-op через
    /// `tool` с заглушкой чтения не нужен — здесь важно только то, что
    /// движок реально дошёл до `Finished`, не побочный эффект в рабочей
    /// области; побочный эффект `files.write` в реальной рабочей области
    /// дочернего процесса проверяется e2e-тестом через настоящий бинарник,
    /// `tests/serve_cli.rs` — здесь неявный `current_dir` теста (корень
    /// пакета, общий для параллельных тестов) не место для мутаций
    /// файловой системы.
    const RUN_PROCESS: &str = r#"
process: serve-run-demo
version: 1
steps:
  - id: noop
    type: tool
    tool: files.list
    args: {path: "."}
limits:
  max_steps: 10
  timeout: 1m
"#;

    #[test]
    fn run_endpoint_executes_trivial_process_and_returns_finished() {
        let mut config = temp_config_with_token("secret-6");
        // См. doc-комментарий модуля: без терминала любое подтверждение
        // (не только human_gate) молча отклоняется — serve обязан
        // работать с confirmation_mode = off. files.list здесь не
        // мутирует, но конфигурация — та, что реально используют
        // операторы serve, не случайное совпадение.
        config.confirmation_mode = berimor_types::capability::ConfirmationMode::Off;
        let dir = config.storage_path.parent().unwrap().to_path_buf();
        let process_path = dir.join("demo.yaml");
        std::fs::write(&process_path, RUN_PROCESS).unwrap();
        let port = start_test_server(&config);
        let body = json!({"process": process_path.to_str().unwrap()}).to_string();
        let request = format!(
            "POST /run HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer secret-6\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let response = raw_request(port, &request);
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        assert!(response.contains("\"finished\""), "{response}");
    }

    #[test]
    fn run_endpoint_rejects_missing_process_field() {
        let config = temp_config_with_token("secret-7");
        let port = start_test_server(&config);
        let body = "{}";
        let request = format!(
            "POST /run HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer secret-7\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let response = raw_request(port, &request);
        assert!(response.starts_with("HTTP/1.1 400"), "{response}");
    }

    #[test]
    fn run_endpoint_rejects_malformed_json_body() {
        let config = temp_config_with_token("secret-8");
        let port = start_test_server(&config);
        let body = "not json";
        let request = format!(
            "POST /run HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer secret-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let response = raw_request(port, &request);
        assert!(response.starts_with("HTTP/1.1 400"), "{response}");
    }
}
