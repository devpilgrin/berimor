//! §20.10: e2e-доказательство встроенных инструментов через реальный
//! `berimor run` — не только юнит-тесты диспетчера.
//!
//! Процесс без единого вызова модели: files.write → files.list →
//! terminal.exec — все три через capability-гейт на реальном пути.
//! Отдельно: deny-статика останавливает `rm -rf /`, jail — чтение за
//! пределами рабочей области (оба класса — не в диспетчере, а в гейте,
//! до исполнения).

use std::path::PathBuf;
use std::process::Command;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_berimor"))
}

const PROCESS: &str = r#"
process: builtin-tools-demo
version: 1
steps:
  - id: write_note
    type: tool
    tool: files.write
    args: {path: "output/note.txt", content: "создано berimor"}
  - id: list_dir
    type: tool
    tool: files.list
    args: {path: "output"}
  - id: run_command
    type: tool
    tool: terminal.exec
    args: {command: "echo hello-from-berimor"}
limits:
  max_steps: 10
  timeout: 1m
  token_budget: 1k
"#;

fn write_config(dir: &std::path::Path, name: &str) -> PathBuf {
    let config_path = dir.join(format!("{name}.toml"));
    std::fs::write(
        &config_path,
        format!("storage_path = \"./{name}.db\"\nconfirmation_mode = \"off\"\n"),
    )
    .unwrap();
    config_path
}

fn run(
    dir: &std::path::Path,
    config: &std::path::Path,
    process: &std::path::Path,
) -> std::process::Output {
    // Изоляция от глобального конфига пользователя (§20.12).
    let empty_xdg = std::env::temp_dir().join(format!("berimor-e2e-xdg-{}", std::process::id()));
    std::fs::create_dir_all(&empty_xdg).unwrap();
    Command::new(bin())
        .arg("--config")
        .arg(config)
        .arg("run")
        .arg(process)
        .current_dir(dir)
        .env("XDG_CONFIG_HOME", &empty_xdg)
        .output()
        .unwrap()
}

#[test]
fn builtin_tools_execute_in_real_cli_run_without_any_model_call() {
    let dir = std::env::temp_dir().join(format!("berimor-e2e-bt-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("output")).unwrap();
    let config_path = write_config(&dir, "bt");
    let process_path = dir.join("process.yaml");
    std::fs::write(&process_path, PROCESS).unwrap();

    let output = run(&dir, &config_path, &process_path);
    assert!(
        output.status.success(),
        "run обязан завершиться: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Побочный эффект реален: файл создан процессом в рабочей области.
    let note = std::fs::read_to_string(dir.join("output/note.txt")).unwrap();
    assert_eq!(note, "создано berimor");

    // Результаты инструментов — в финальном состоянии.
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("note.txt"), "листинг в состоянии: {stdout}");
    assert!(
        stdout.contains("hello-from-berimor"),
        "stdout терминала в состоянии: {stdout}"
    );
}

#[test]
fn deny_static_stops_rm_rf_before_any_execution() {
    let dir = std::env::temp_dir().join(format!("berimor-e2e-btdeny-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let config_path = write_config(&dir, "btdeny");
    let process_path = dir.join("process.yaml");
    std::fs::write(
        &process_path,
        r#"
process: deny-demo
version: 1
steps:
  - id: dangerous
    type: tool
    tool: terminal.exec
    args: {command: "rm -rf /"}
limits:
  max_steps: 10
  timeout: 1m
  token_budget: 1k
"#,
    )
    .unwrap();

    let output = run(&dir, &config_path, &process_path);
    assert!(!output.status.success(), "deny обязан остановить процесс");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("заблокирован") || stderr.contains("deny") || stderr.contains("Deny"),
        "причина — deny-статика: {stderr}"
    );
}

#[test]
fn jail_stops_read_outside_workspace() {
    let dir = std::env::temp_dir().join(format!("berimor-e2e-btjail-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    // Файл-жертва ЗА пределами рабочей области.
    let outside = dir
        .join("..")
        .join(format!("outside-{}.txt", std::process::id()));
    let outside = outside.canonicalize().unwrap_or_else(|_| {
        let p = std::env::temp_dir().join(format!("outside-{}.txt", std::process::id()));
        p
    });
    std::fs::write(&outside, "секрет вне области").unwrap();

    let config_path = write_config(&dir, "btjail");
    let process_path = dir.join("process.yaml");
    std::fs::write(
        &process_path,
        format!(
            r#"
process: jail-demo
version: 1
steps:
  - id: escape
    type: tool
    tool: files.read
    args: {{path: "{}"}}
limits:
  max_steps: 10
  timeout: 1m
  token_budget: 1k
"#,
            outside.display()
        ),
    )
    .unwrap();

    let output = run(&dir, &config_path, &process_path);
    assert!(!output.status.success(), "jail обязан остановить процесс");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("заблокирован") || stderr.contains("jail") || stderr.contains("области"),
        "причина — jail: {stderr}"
    );
    std::fs::remove_file(&outside).ok();
}
