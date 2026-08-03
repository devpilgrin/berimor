//! CLI-M3: e2e через реальный бинарник `berimor` для подкоманд `trace`
//! (O1) и `eval` (O2) — обе читают journal/golden-набор, который сама же
//! `run` уже пишет, тем же путём, что и `tests/e2e_run.rs` (не вызов
//! функций напрямую).

use std::path::PathBuf;
use std::process::{Command, Stdio};

const SINGLE_TOOL_PROCESS: &str = r#"
process: single-tool
version: 1
steps:
  - id: fetch
    type: tool
    tool: lookup
    args: {id: "{{state.user.id}}"}
limits:
  max_steps: 10
  timeout: 1m
"#;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_berimor"))
}

fn temp_dir(name: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("berimor-observe-e2e-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_config(dir: &std::path::Path) -> PathBuf {
    let path = dir.join("config.toml");
    let storage = dir.join("run.db");
    let contents = format!(
        r#"
storage_path = "{storage}"
confirmation_mode = "smart"

[[tool_stubs]]
tool = "lookup"
mutates = false
response = {{ status = "ok" }}
"#,
        storage = storage.to_string_lossy().replace('\\', "\\\\"),
    );
    std::fs::write(&path, contents).unwrap();
    path
}

fn write_process_file(dir: &std::path::Path) -> PathBuf {
    let path = dir.join("process.yaml");
    std::fs::write(&path, SINGLE_TOOL_PROCESS).unwrap();
    path
}

fn run_cli(args: &[&str], config: &std::path::Path) -> (bool, String, String) {
    let output = Command::new(bin())
        .arg("--config")
        .arg(config)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("XDG_CONFIG_HOME", "/nonexistent-berimor-e2e-xdg") // изоляция от глобального конфига (§20.12)
        .output()
        .expect("бинарник berimor собран (cargo test)");
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

fn extract_instance_id(stdout: &str) -> String {
    let marker = "[berimor] создан инстанс ";
    stdout
        .lines()
        .find_map(|line| line.strip_prefix(marker))
        .unwrap_or_else(|| panic!("не найдена строка с id инстанса:\n{stdout}"))
        .trim()
        .to_string()
}

#[test]
fn trace_prints_journaled_events_for_a_completed_instance() {
    let dir = temp_dir("trace");
    let config = write_config(&dir);
    let process = write_process_file(&dir);

    let (ok, stdout, stderr) = run_cli(
        &[
            "run",
            process.to_str().unwrap(),
            "--input",
            r#"{"user": {"id": "u-1"}}"#,
        ],
        &config,
    );
    assert!(ok, "прогон обязан завершиться успехом:\n{stdout}\n{stderr}");
    let instance_id = extract_instance_id(&stdout);

    let (ok, stdout, stderr) = run_cli(&["trace", &instance_id], &config);
    assert!(ok, "trace обязан завершиться успехом:\n{stdout}\n{stderr}");
    assert!(
        stdout.contains("instantiated"),
        "нет instantiated:\n{stdout}"
    );
    assert!(
        stdout.contains("step_applied") && stdout.contains("fetch"),
        "нет применения шага 'fetch':\n{stdout}"
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn trace_of_unknown_instance_prints_a_message_not_an_error() {
    let dir = temp_dir("trace-unknown");
    let config = write_config(&dir);
    // Журнал ещё не существует — trace обязан открыть/создать его сам,
    // как и `run`, не падать на отсутствующем файле.
    let (ok, stdout, stderr) = run_cli(&["trace", "no-such-instance"], &config);
    assert!(
        ok,
        "trace неизвестного инстанса не ошибка:\n{stdout}\n{stderr}"
    );
    assert!(stdout.contains("не найден"));

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn eval_runs_every_scenario_and_prints_branch_coverage() {
    let dir = temp_dir("eval");
    let config = write_config(&dir);
    let golden_dir = dir.join("golden");
    std::fs::create_dir_all(&golden_dir).unwrap();
    std::fs::write(golden_dir.join("process.yaml"), SINGLE_TOOL_PROCESS).unwrap();
    std::fs::write(
        golden_dir.join("scenario-a.json"),
        serde_json::to_string(&serde_json::json!({"user": {"id": "u-1"}})).unwrap(),
    )
    .unwrap();
    std::fs::write(
        golden_dir.join("scenario-b.json"),
        serde_json::to_string(&serde_json::json!({"user": {"id": "u-2"}})).unwrap(),
    )
    .unwrap();

    let (ok, stdout, stderr) = run_cli(&["eval", golden_dir.to_str().unwrap()], &config);
    assert!(ok, "eval обязан завершиться успехом:\n{stdout}\n{stderr}");

    assert!(
        stdout.contains("2 сценариев"),
        "неверное число сценариев:\n{stdout}"
    );
    assert!(stdout.contains("scenario-a"));
    assert!(stdout.contains("scenario-b"));
    assert!(
        stdout.contains("finished"),
        "оба сценария без branch-шагов обязаны дойти до Finished:\n{stdout}"
    );
    assert!(
        stdout.contains("доля веток: 1.00"),
        "в процессе нет branch-шагов — покрытие обязано быть полным по определению:\n{stdout}"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// `eval` обязан работать на эфемерном журнале, не на `config.storage_path`
/// — два подряд запуска против одного и того же конфига не должны ни
/// давать разные метрики, ни оставлять след в реальном журнале прогонов
/// (найдено независимым ревью интеграции CLI-M1/M2/M3: до фикса
/// `run_golden_set` копил события повторных eval-прогонов под одним и
/// тем же детерминированным instance_id в общем `storage_path`).
#[test]
fn eval_does_not_pollute_the_real_run_journal_across_repeated_invocations() {
    let dir = temp_dir("eval-no-pollution");
    let config = write_config(&dir);
    let golden_dir = dir.join("golden");
    std::fs::create_dir_all(&golden_dir).unwrap();
    std::fs::write(golden_dir.join("process.yaml"), SINGLE_TOOL_PROCESS).unwrap();
    std::fs::write(
        golden_dir.join("scenario-a.json"),
        serde_json::to_string(&serde_json::json!({"user": {"id": "u-1"}})).unwrap(),
    )
    .unwrap();

    let (ok1, stdout1, stderr1) = run_cli(&["eval", golden_dir.to_str().unwrap()], &config);
    assert!(
        ok1,
        "первый eval обязан завершиться успехом:\n{stdout1}\n{stderr1}"
    );
    let (ok2, stdout2, stderr2) = run_cli(&["eval", golden_dir.to_str().unwrap()], &config);
    assert!(
        ok2,
        "второй eval обязан завершиться успехом:\n{stdout2}\n{stderr2}"
    );
    assert_eq!(
        stdout1, stdout2,
        "повторный eval против того же config.toml обязан давать идентичный отчёт"
    );

    // Реальный журнал прогонов (тот же storage_path, что видит `run`)
    // не должен знать об инстансах eval-сценариев вообще.
    let (ok, stdout, stderr) = run_cli(&["trace", "single-tool::scenario-a"], &config);
    assert!(ok, "trace обязан завершиться успехом:\n{stdout}\n{stderr}");
    assert!(
        stdout.contains("не найден"),
        "eval не должен писать в реальный журнал storage_path:\n{stdout}"
    );

    std::fs::remove_dir_all(&dir).ok();
}
