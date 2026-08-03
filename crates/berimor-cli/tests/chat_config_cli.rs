//! §20.12: e2e слоистой конфигурации и команд чата через реальный
//! бинарник: /help, /config, /models без провайдеров (модель не
//! вызывается — команды служебные), полный цикл /models add: пресет →
//! глобальный конфиг + secrets.env (0600) в подменённом XDG_CONFIG_HOME
//! → перезагрузка рантайма → /models видит добавленный провайдер.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_berimor"))
}

struct Sandbox {
    dir: PathBuf,
    xdg: PathBuf,
}

fn sandbox(tag: &str) -> Sandbox {
    let root = std::env::temp_dir().join(format!("berimor-e2e-{tag}-{}", std::process::id()));
    let dir = root.join("work");
    let xdg = root.join("xdg");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::create_dir_all(&xdg).unwrap();
    // Локальный конфиг без провайдеров — команды чата модели не требуют.
    std::fs::write(
        dir.join("berimor.toml"),
        "storage_path = \"./chat.db\"\nconfirmation_mode = \"off\"\n",
    )
    .unwrap();
    Sandbox { dir, xdg }
}

fn run_chat(sandbox: &Sandbox, input: &str) -> Output {
    let mut child = Command::new(bin())
        .arg("chat")
        .current_dir(&sandbox.dir)
        .env("XDG_CONFIG_HOME", &sandbox.xdg)
        // Детерминизм мастера: ключ провайдера в окружении хоста меняет
        // сценарий (пропуск вопроса о ключе → сдвиг пайп-ввода).
        // Поймано на машине разработчика 2026-08-03.
        .env_remove("DEEPSEEK_API_KEY")
        .env_remove("MOONSHOT_API_KEY")
        .env_remove("OPENAI_API_KEY")
        .env_remove("OPENROUTER_API_KEY")
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
    child.wait_with_output().unwrap()
}

#[test]
fn slash_commands_work_without_any_provider() {
    let sandbox = sandbox("slash");
    let output = run_chat(&sandbox, "/help\n/config\n/models\n/exit\n");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("/models add"),
        "/help перечисляет команды: {stderr}"
    );
    assert!(
        stderr.contains("режим подтверждений"),
        "/config показывает эффективный конфиг: {stderr}"
    );
    assert!(
        stderr.contains("провайдеры не настроены"),
        "/models честен о пустом конфиге: {stderr}"
    );
}

#[cfg(unix)]
#[test]
fn models_add_full_cycle_writes_global_config_and_reloads_runtime() {
    use std::os::unix::fs::PermissionsExt;
    let sandbox = sandbox("modelsadd");
    // deepseek — пресет №2; дальше мастер спрашивает model_id (Enter =
    // умолчание) и ключ API; затем /models после перезагрузки рантайма.
    let output = run_chat(
        &sandbox,
        "/models add\n2\n\ntest-deepseek-key\n/models\n/exit\n",
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Глобальный конфиг создан мастером.
    let global = sandbox.xdg.join("berimor/config.toml");
    let text = std::fs::read_to_string(&global).unwrap();
    assert!(text.contains("name = \"deepseek\""), "{text}");
    assert!(text.contains("api.deepseek.com"), "{text}");
    assert!(
        text.contains("api_key_env = \"DEEPSEEK_API_KEY\""),
        "{text}"
    );

    // Ключ — в secrets.env с правами владельца, НЕ в конфиге.
    let secrets = sandbox.xdg.join("berimor/secrets.env");
    let secrets_text = std::fs::read_to_string(&secrets).unwrap();
    assert_eq!(secrets_text, "DEEPSEEK_API_KEY=test-deepseek-key\n");
    let mode = std::fs::metadata(&secrets).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600, "{mode:o}");
    assert!(!text.contains("test-deepseek-key"), "ключа в конфиге нет");

    // Рантайм перезагрузился: /models видит провайдера без рестарта.
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("конфигурация перечитана"),
        "перезагрузка после add: {stderr}"
    );
    assert!(
        stderr.contains("deepseek — deepseek-chat"),
        "/models после перезагрузки: {stderr}"
    );
}

#[test]
fn unknown_slash_command_is_guided_to_help() {
    let sandbox = sandbox("unknown");
    let output = run_chat(&sandbox, "/nosuch\n/exit\n");
    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("неизвестная команда"), "{stderr}");
}
