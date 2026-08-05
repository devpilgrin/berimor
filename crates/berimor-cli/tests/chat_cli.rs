//! §20.11: e2e-доказательство интерактивного режима `berimor chat` —
//! REPL поверх AgentStep со встроенными инструментами через реальный
//! бинарник, с stdin из пайпа и мок-провайдером по очереди ответов.
//!
//! Сценарии: простой ответ без инструментов; ход с реальным вызовом
//! files.write (побочный эффект в рабочей области); отказ гейта
//! (`rm -rf /`) — наблюдением агенту, не смертью сессии.

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Command, Stdio};

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_berimor"))
}

/// Мок, отвечающий телами `bodies` по очереди подключений (каждый ход
/// агента — отдельный HTTP-вызов).
fn sequential_mock(bodies: Vec<Value>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for body in bodies {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream);
            let mut content_length = 0usize;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line.to_lowercase().starts_with("content-length:") {
                    content_length = line[15..].trim().parse().unwrap();
                }
                if line.trim().is_empty() {
                    break;
                }
            }
            let mut request_body = vec![0u8; content_length];
            reader.read_exact(&mut request_body).unwrap();

            let content = serde_json::to_string(&body).unwrap();
            let envelope = json!({"choices": [{"message": {"content": content}}]}).to_string();
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{envelope}",
                envelope.len()
            );
            reader.get_mut().write_all(response.as_bytes()).unwrap();
        }
    });
    url
}

fn finish_turn(reply: &str) -> Value {
    json!({
        "thought": "Отвечаю пользователю.",
        "action": {"kind": "finish", "result": {"reply": reply}}
    })
}

fn tool_turn(tool: &str, args: Value) -> Value {
    json!({
        "thought": "Нужно выполнить действие.",
        "action": {"kind": "tool", "tool": tool, "args": args}
    })
}

fn write_config(dir: &std::path::Path, name: &str, mock_url: &str) -> PathBuf {
    let config_path = dir.join(format!("{name}.toml"));
    std::fs::write(
        &config_path,
        format!(
            r#"storage_path = "./{name}.db"
confirmation_mode = "off"

[[providers]]
name = "mock"
model_id = "mock-model"
tier = "strong"
base_url = "{mock_url}"
api_key = "mock-key"
allow_private_endpoint = true
"#
        ),
    )
    .unwrap();
    config_path
}

fn run_chat(dir: &std::path::Path, config: &std::path::Path, input: &str) -> std::process::Output {
    // Изоляция от глобального конфига пользователя (§20.12): иначе
    // ~/.config/berimor/config.toml подмешивал бы реальные провайдеры
    // в тесты на машине разработчика.
    let empty_xdg = std::env::temp_dir().join(format!("berimor-e2e-xdg-{}", std::process::id()));
    std::fs::create_dir_all(&empty_xdg).unwrap();
    // Изоляция и от установленных плагинов разработчика: plugins_root —
    // в data_dir (§20.18); e2e сажает свои плагины в XDG_DATA_HOME.
    let empty_data = std::env::temp_dir().join(format!("berimor-e2e-data-{}", std::process::id()));
    std::fs::create_dir_all(&empty_data).unwrap();
    let mut child = Command::new(bin())
        .arg("--config")
        .arg(config)
        .arg("chat")
        .current_dir(dir)
        .env("XDG_CONFIG_HOME", &empty_xdg)
        .env("XDG_DATA_HOME", &empty_data)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    // stdin закрывается при drop — для REPL это EOF, сессия завершается.
    child.wait_with_output().unwrap()
}

#[test]
fn chat_answers_simple_message_and_exits_on_eof() {
    let dir = std::env::temp_dir().join(format!("berimor-e2e-chat-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let url = sequential_mock(vec![finish_turn("Здравствуйте, сэр.")]);
    let config_path = write_config(&dir, "chat", &url);

    let output = run_chat(&dir, &config_path, "привет\n");
    assert!(
        output.status.success(),
        "chat обязан завершиться по EOF: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Здравствуйте, сэр."),
        "ответ модели пользователю: {stdout}"
    );
    assert!(stdout.contains("berimor"), "метка агента: {stdout}");
}

#[test]
fn chat_executes_builtin_tool_with_real_side_effect() {
    let dir = std::env::temp_dir().join(format!("berimor-e2e-chattool-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("output")).unwrap();
    let url = sequential_mock(vec![
        tool_turn(
            "files.write",
            json!({"path": "output/from-chat.txt", "content": "написано агентом"}),
        ),
        finish_turn("Файл создан."),
    ]);
    let config_path = write_config(&dir, "chattool", &url);

    let output = run_chat(&dir, &config_path, "создай файл\n/exit\n");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let written = std::fs::read_to_string(dir.join("output/from-chat.txt")).unwrap();
    assert_eq!(written, "написано агентом");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Файл создан."), "{stdout}");
}

#[test]
fn skill_trigger_injects_body_and_enforces_tool_ceiling() {
    // §20.16: триггер фразы — кодом; тело скилла — системным контекстом
    // хода; потолок инструментов — фильтром диспетча. Скилл разрешает
    // только files.read: terminal.exec обязан быть отклонён фильтром с
    // говорящей причиной (модель получает её наблюдением).
    let dir = std::env::temp_dir().join(format!("berimor-e2e-skill-{}", std::process::id()));
    let skill_dir = dir.join(".berimor/skills/reviewer");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: reviewer\nversion: 0.1.0\ndescription: Ревьюер\ntriggers:\n  - \"ревью\"\ntools:\n  - files.read\n---\n\n# Инструкции ревьюера\n",
    )
    .unwrap();
    let url = sequential_mock(vec![
        // Побочный эффект вне потолка скилла: если фильтр не сработает,
        // файл появится (confirmation_mode = off — конфирмер не мешает).
        tool_turn(
            "terminal.exec",
            json!({"command": "touch escaped-skill.txt"}),
        ),
        finish_turn("Понял, работаю в потолке."),
    ]);
    let config_path = write_config(&dir, "chatskill", &url);

    let output = run_chat(&dir, &config_path, "ревью проекта\n/exit\n");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("скилл «reviewer» активен"),
        "заметка об активации: {stderr}"
    );
    // Доказательство фильтра потолка — отсутствие побочного эффекта.
    assert!(
        !dir.join("escaped-skill.txt").exists(),
        "файл не должен быть создан: вызов вне потолка скилла отклонён"
    );
    assert!(
        stderr.contains("✗") && stderr.contains("terminal.exec"),
        "ход terminal.exec отклонён потолком: {stderr}"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Понял, работаю в потолке."),
        "цикл дошёл до ответа: {stdout}"
    );
}
#[test]
fn subagent_runs_nested_loop_and_reports_back() {
    // §20.17: agents.run исполняет поручение вложенным циклом AgentStep
    // (тот же мок-провайдер отвечает по очереди: родитель → ребёнок →
    // родитель); итог ребёнка — наблюдение родителю.
    let dir = std::env::temp_dir().join(format!("berimor-e2e-agentrun-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let url = sequential_mock(vec![
        tool_turn(
            "agents.run",
            json!({"name": "scout", "task": "найди файлы"}),
        ),
        finish_turn("Отчёт скаута: 3 файла."),
        finish_turn("Готово: субагент доложил."),
    ]);
    let agent_dir = dir.join(".berimor/agents/scout");
    std::fs::create_dir_all(&agent_dir).unwrap();
    std::fs::write(
        agent_dir.join("agent.yaml"),
        "name: scout\ndescription: разведчик\ntools:\n  - files.list\nbudget:\n  max_turns: 4\n",
    )
    .unwrap();
    let config_path = write_config(&dir, "agentrun", &url);

    let output = run_chat(&dir, &config_path, "поручи скауту разведку\n/exit\n");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("✓") && stderr.contains("agents.run"),
        "ход agents.run успешен: {stderr}"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("субагент доложил"),
        "финальный ответ родителя: {stdout}"
    );
}

#[test]
fn subagent_tool_ceiling_rejects_out_of_scope_tool() {
    // §20.17: потолок ребёнка — agent.tools ∩ права родителя; вызов вне
    // потолка отклоняется кодом (✗), без потолка `ls` прошёл бы.
    let dir = std::env::temp_dir().join(format!("berimor-e2e-agentceil-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let url = sequential_mock(vec![
        tool_turn(
            "agents.run",
            json!({"name": "scout", "task": "проверь диск"}),
        ),
        // Попытка побочного эффекта ВНЕ потолка: если фильтр не сработает,
        // файл появится (confirmation_mode = off — конфирмер не мешает).
        tool_turn(
            "terminal.exec",
            json!({"command": "touch escaped-subagent.txt"}),
        ),
        finish_turn("Потолок не пускает: только files.list."),
        finish_turn("Субагент честно доложил о границе."),
    ]);
    let agent_dir = dir.join(".berimor/agents/scout");
    std::fs::create_dir_all(&agent_dir).unwrap();
    std::fs::write(
        agent_dir.join("agent.yaml"),
        "name: scout\ntools:\n  - files.list\nbudget:\n  max_turns: 4\n",
    )
    .unwrap();
    let config_path = write_config(&dir, "agentceil", &url);

    let output = run_chat(&dir, &config_path, "поручи скауту\n/exit\n");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    // Доказательство потолка — отсутствие побочного эффекта.
    assert!(
        !dir.join("escaped-subagent.txt").exists(),
        "файл не должен быть создан: вызов вне потолка отклонён кодом"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("границе"),
        "цепочка дошла до финального ответа: {stdout}"
    );
}

#[test]
fn chat_survives_gate_denial_turn_terminal_session_alive() {
    // Отказ гейта (deny-статика) ТЕРМИНАЛЕН для хода (осознанный дизайн),
    // но НЕ для сессии: следующее сообщение обрабатывается штатно.
    // Доказательство — ответ на второе сообщение доходит.
    let dir = std::env::temp_dir().join(format!("berimor-e2e-deny-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let url = sequential_mock(vec![
        // Ход 1: опасная команда — гейт отклоняет, ход завершается отказом.
        tool_turn("terminal.exec", json!({"command": "rm -rf /"})),
        // Ход 2 (новое сообщение): сессия жива — ответ доходит.
        finish_turn("Сессия жива, сэр."),
    ]);
    let config_path = write_config(&dir, "chatdeny", &url);

    let output = run_chat(&dir, &config_path, "удали всё\nещё раз здравствуй\n/exit\n");
    assert!(
        output.status.success(),
        "сессия переживает отказ гейта: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Сессия жива"),
        "ответ на сообщение ПОСЛЕ отказа гейта: {stdout}"
    );
}

#[test]
fn plugin_tool_callable_from_chat() {
    // §20.18: установленный плагин — инструмент первого класса: модель
    // видит его в каталоге, вызов — процессом (JSON stdin/stdout),
    // ответ — наблюдением. mutates: false из манифеста.
    let dir = std::env::temp_dir().join(format!("berimor-e2e-plugin-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let url = sequential_mock(vec![
        tool_turn("hello.greet", json!({"name": "беримор"})),
        finish_turn("Плагин поздоровался: Привет, беримор."),
    ]);
    // Сажаем плагин в изолированный XDG_DATA_HOME (тот же pid-путь, что
    // в run_chat).
    let plugin_dir = std::env::temp_dir().join(format!(
        "berimor-e2e-data-{}/berimor/plugins/installed/hello",
        std::process::id()
    ));
    std::fs::create_dir_all(&plugin_dir).unwrap();
    std::fs::write(
        plugin_dir.join("manifest.yaml"),
        "name: hello\ncapabilities:\n  tools:\n    - name: hello.greet\n      description: Приветствие\n      mutates: false\n",
    )
    .unwrap();
    std::fs::write(
        plugin_dir.join("hello"),
        "#!/bin/sh\nread -r args\necho '{\"content\": \"Привет, беримор\"}'\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(
            plugin_dir.join("hello"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }
    let config_path = write_config(&dir, "chatplugin", &url);

    let output = run_chat(&dir, &config_path, "поздоровайся плагином\n/exit\n");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("✓") && stderr.contains("hello.greet"),
        "вызов инструмента плагина успешен: {stderr}"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Плагин поздоровался"),
        "ответ с данными плагина: {stdout}"
    );
}
