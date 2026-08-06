//! §20.19: e2e установки расширений из произвольных git-репозиториев и
//! локальных источников — через реальный бинарник, без сети (локальный
//! git-репозиторий как remote).

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_berimor"))
}

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("berimor-e2e-ext-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Запуск с изоляцией XDG (глобальные установки — в temp).
fn run(args: &[&str], xdg: &Path) -> Output {
    Command::new(bin())
        .args(args)
        .env("XDG_CONFIG_HOME", xdg.join("config"))
        .env("XDG_DATA_HOME", xdg.join("data"))
        .output()
        .unwrap()
}

fn git_init_repo(dir: &Path) {
    let git = |args: &[&str]| {
        let output = Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "e2e")
            .env("GIT_AUTHOR_EMAIL", "e2e@test")
            .env("GIT_COMMITTER_NAME", "e2e")
            .env("GIT_COMMITTER_EMAIL", "e2e@test")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    git(&["init", "-q", "-b", "main"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "e2e"]);
}

const SKILL_MD: &str = "---\nname: my-skill\nversion: 0.1.0\ndescription: Тестовый\ntriggers:\n  - \"тест\"\ntools:\n  - files.read\n---\n\n# Инструкции\n";

#[test]
fn skill_install_from_git_repo_root() {
    // Репозиторий, который САМ является скиллом (SKILL.md в корне).
    let repo = temp_dir("skillroot");
    std::fs::write(repo.join("SKILL.md"), SKILL_MD).unwrap();
    git_init_repo(&repo);
    let xdg = temp_dir("xdg-skillroot");

    let output = run(
        &[
            "skill",
            "install",
            "my-skill",
            "--from",
            repo.to_str().unwrap(),
        ],
        &xdg,
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let installed = xdg.join("config/berimor/skills/my-skill/SKILL.md");
    assert!(
        installed.is_file(),
        "скилл установлен глобально: {installed:?}"
    );
}

#[test]
fn skill_install_from_git_with_subdir() {
    // Манифест в подкаталоге (--path).
    let repo = temp_dir("skillpath");
    std::fs::create_dir_all(repo.join("pack/review")).unwrap();
    std::fs::write(repo.join("pack/review/SKILL.md"), SKILL_MD).unwrap();
    git_init_repo(&repo);
    let xdg = temp_dir("xdg-skillpath");

    let output = run(
        &[
            "skill",
            "install",
            "my-skill",
            "--from",
            repo.to_str().unwrap(),
            "--path",
            "pack/review",
        ],
        &xdg,
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(xdg
        .join("config/berimor/skills/my-skill/SKILL.md")
        .is_file());
}

#[test]
fn agent_install_from_git_repo_root() {
    let repo = temp_dir("agentroot");
    std::fs::write(
        repo.join("agent.yaml"),
        "name: scout\ndescription: разведчик\ntools:\n  - files.list\n",
    )
    .unwrap();
    std::fs::write(repo.join("prompt.md"), "Ты разведчик.").unwrap();
    git_init_repo(&repo);
    let xdg = temp_dir("xdg-agentroot");

    let output = run(
        &[
            "agent",
            "install",
            "scout",
            "--from",
            repo.to_str().unwrap(),
        ],
        &xdg,
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(xdg.join("config/berimor/agents/scout/agent.yaml").is_file());
    assert!(xdg.join("config/berimor/agents/scout/prompt.md").is_file());
}

#[test]
fn plugin_install_local_requires_allow_unsigned() {
    let plugin = temp_dir("plugindir");
    std::fs::write(
        plugin.join("manifest.yaml"),
        "name: local-hello\ncapabilities:\n  tools:\n    - name: local.hello\n      description: Локальный\n      mutates: false\n",
    )
    .unwrap();
    std::fs::write(
        plugin.join("local-hello"),
        "#!/bin/sh\nread -r a\necho '{\"content\": \"ok\"}'\n",
    )
    .unwrap();
    let xdg = temp_dir("xdg-pluginlocal");

    // Без флага — отказ с объяснением.
    let output = run(&["plugin", "install-local", plugin.to_str().unwrap()], &xdg);
    assert!(!output.status.success(), "без флага — отказ");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("--allow-unsigned"),
        "говорящий отказ: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // С флагом — установка, бинарник исполняемый.
    let output = run(
        &[
            "plugin",
            "install-local",
            plugin.to_str().unwrap(),
            "--allow-unsigned",
        ],
        &xdg,
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let binary = xdg.join("data/berimor/plugins/installed/local-hello/local-hello");
    assert!(binary.is_file(), "бинарник установлен: {binary:?}");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&binary).unwrap().permissions().mode();
        assert!(mode & 0o111 != 0, "исполняемый бит: {mode:o}");
    }
    assert!(xdg
        .join("data/berimor/plugins/manifests/local-hello.yaml")
        .is_file());
}

#[test]
fn plugin_install_local_from_git_repo() {
    let repo = temp_dir("plugingit");
    std::fs::write(
        repo.join("manifest.yaml"),
        "name: git-hello\ncapabilities:\n  tools:\n    - name: git.hello\n      mutates: false\n",
    )
    .unwrap();
    std::fs::write(
        repo.join("git-hello"),
        "#!/bin/sh\nread -r a\necho '{\"content\": \"ok\"}'\n",
    )
    .unwrap();
    git_init_repo(&repo);
    let xdg = temp_dir("xdg-plugingit");

    let output = run(
        &[
            "plugin",
            "install-local",
            repo.to_str().unwrap(),
            "--allow-unsigned",
        ],
        &xdg,
    );
    // git clone по локальному пути (без ://) трактуется как каталог —
    // тоже валидный источник.
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(xdg
        .join("data/berimor/plugins/installed/git-hello/manifest.yaml")
        .is_file());
}
