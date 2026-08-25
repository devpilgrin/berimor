//! Landlock-песочница для `terminal.exec` (0.36.0; перенос идеи
//! deepseek-ai/deepseek-harness native/landlock-run — self-restrict-
//! then-exec — на Rust через libc, без внешнего бинаря: цепочка
//! поставки остаётся нашей, подписанной).
//!
//! Модель: в pre_exec порождённого процесса устанавливается ruleset —
//! рабочая область RW, системные каталоги RO, /tmp и /dev RW; затем
//! `no_new_privs` + `restrict_self`. Ruleset наследуется через execve и
//! всеми потомками: команда и всё, что она порождает, физически не
//! выходит за область. Сетевые сокеты Landlock ABI до v4 не покрывает —
//! сеть по-прежнему сторожит capability-гейт (allow-лист хостов).
//!
//! Режимы (`[sandbox] landlock`):
//! - "off" — не применять;
//! - "auto" (дефолт) — применять, если ядро умеет Landlock; иначе —
//!   одно предупреждение в stderr и запуск без песочницы (прежнее
//!   поведение, платформенная честность: macOS/Windows — без изменений);
//! - "require" — fail-closed: нет Landlock — команда не запускается.

use std::io;
use std::path::Path;

/// Режим песочницы.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LandlockMode {
    Off,
    #[default]
    Auto,
    Require,
}

impl LandlockMode {
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "off" => Ok(Self::Off),
            "auto" => Ok(Self::Auto),
            "require" => Ok(Self::Require),
            other => Err(format!(
                "sandbox.landlock: ожидается off|auto|require, получено '{other}'"
            )),
        }
    }
}

/// Обвязка команды песочницей (общая точка для terminal.exec и
/// terminal.start): auto — при поддержке ядра (иначе одно
/// предупреждение и прежнее поведение); require — fail-closed.
/// Сеть (волна H): restrict требует ABI 4; на старом ядре auto —
/// предупреждение и пропуск сетевых правил, require — отказ.
/// На не-Linux тело no-op, параметры намеренно не используются.
#[cfg_attr(not(target_os = "linux"), allow(unused_variables))]
pub fn apply(
    command: &mut std::process::Command,
    workspace: &Path,
    mode: LandlockMode,
    net: &NetPolicy,
) -> Result<(), String> {
    if mode == LandlockMode::Off || !cfg!(target_os = "linux") {
        return Ok(());
    }
    if !kernel_supports() {
        if mode == LandlockMode::Require {
            return Err("sandbox.landlock=require: ядро без Landlock — fail-closed".into());
        }
        static WARNED: std::sync::Once = std::sync::Once::new();
        WARNED.call_once(|| {
            eprintln!(
                "[berimor] sandbox: ядро без Landlock — подпроцессы без песочницы (режим auto)"
            );
        });
        return Ok(());
    }
    #[cfg(target_os = "linux")]
    {
        // Сеть запрошена, а ABI старый: по режиму.
        let net = if net.is_restrict() && kernel_abi() < 4 {
            if mode == LandlockMode::Require {
                return Err(
                    "sandbox.network=restrict требует Landlock ABI 4+ (ядро 6.7+) — fail-closed"
                        .into(),
                );
            }
            static NET_WARNED: std::sync::Once = std::sync::Once::new();
            NET_WARNED.call_once(|| {
                eprintln!(
                    "[berimor] sandbox: ядро без Landlock ABI 4 — сетевые правила пропущены (режим auto)"
                );
            });
            &NetPolicy::Off
        } else {
            net
        };
        let rules = workspace_rules(workspace)
            .map_err(|e| format!("landlock: не удалось собрать правила: {e}"))?;
        let net = net.clone();
        unsafe {
            use std::os::unix::process::CommandExt;
            command.pre_exec(move || confine_current_process_net(&rules, &net));
        }
    }
    Ok(())
}

/// Ядро умеет Landlock (версия ABI ≥ 1). Кэшируется одним probe.
#[cfg(target_os = "linux")]
pub fn kernel_supports() -> bool {
    use std::sync::OnceLock;
    static SUPPORT: OnceLock<bool> = OnceLock::new();
    *SUPPORT.get_or_init(|| {
        // LANDLOCK_CREATE_RULESET_VERSION = 1U << 0
        let abi = unsafe {
            libc::syscall(
                libc::SYS_landlock_create_ruleset,
                std::ptr::null::<libc::c_void>(),
                0usize,
                1u32,
            )
        };
        abi >= 1
    })
}

#[cfg(not(target_os = "linux"))]
pub fn kernel_supports() -> bool {
    false
}

/// Версия ABI Landlock (0 = нет поддержки). Сеть — с ABI 4 (ядро 6.7+).
#[cfg(target_os = "linux")]
pub fn kernel_abi() -> u32 {
    use std::sync::OnceLock;
    static ABI: OnceLock<u32> = OnceLock::new();
    *ABI.get_or_init(|| {
        let abi = unsafe {
            libc::syscall(
                libc::SYS_landlock_create_ruleset,
                std::ptr::null::<libc::c_void>(),
                0usize,
                1u32,
            )
        };
        if abi < 0 {
            0
        } else {
            abi as u32
        }
    })
}

#[cfg(not(target_os = "linux"))]
pub fn kernel_abi() -> u32 {
    0
}

/// Сетевая политика песочницы (волна H, 0.45.0): только TCP, только по
/// портам (так устроен Landlock). Off — сеть не ограничивается вовсе
/// (прежнее поведение); Restrict — разрешены только перечисленные порты.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum NetPolicy {
    #[default]
    Off,
    Restrict {
        allow_connect: Vec<u16>,
        allow_bind: Vec<u16>,
    },
}

impl NetPolicy {
    pub fn is_restrict(&self) -> bool {
        matches!(self, NetPolicy::Restrict { .. })
    }
}

/// Установить правила и замкнуть процесс. Вызывается ИЗ pre_exec —
/// только async-signal-safe операции (libc-вызовы; аллокации допущены
/// до вызова — все строки/пути готовятся снаружи и передаются как
/// CString).
#[cfg(target_os = "linux")]
#[allow(dead_code)] // фасад без сети: зовут тесты; рабочий путь — _net
pub fn confine_current_process(rules: &[(std::ffi::CString, u64)]) -> io::Result<()> {
    confine_current_process_net(rules, &NetPolicy::Off)
}

/// То же + сетевая политика (ABI 4+, ядро 6.7+). Restrict на старом
/// ядре — ошибка Unsupported: вызывающий решает по режиму (auto —
/// предупреждение и пропуск сетевых правил; require — fail-closed).
#[cfg(target_os = "linux")]
pub fn confine_current_process_net(
    rules: &[(std::ffi::CString, u64)],
    net: &NetPolicy,
) -> io::Result<()> {
    // ABI, зажатый до поддерживаемого ядром (пересечение).
    let abi = kernel_abi() as i64;
    if abi < 1 {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "ядро без Landlock",
        ));
    }
    let want_net = net.is_restrict();
    if want_net && abi < 4 {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "сетевые правила требуют Landlock ABI 4+ (ядро 6.7+)",
        ));
    }
    // FS-права появились в v1; REFER — v2, TRUNCATE — v3; сеть — v4.
    // Пересекаем обрабатываемый набор с возможностями ядра (зажим до v3,
    // если сеть не запрошена или ядро её не умеет).
    let abi = if want_net { abi.min(4) } else { abi.min(3) };

    const FS_EXECUTE: u64 = 1 << 0;
    const FS_WRITE_FILE: u64 = 1 << 1;
    const FS_READ_FILE: u64 = 1 << 2;
    const FS_READ_DIR: u64 = 1 << 3;
    const FS_REMOVE_DIR: u64 = 1 << 4;
    const FS_REMOVE_FILE: u64 = 1 << 5;
    const FS_MAKE_CHAR: u64 = 1 << 6;
    const FS_MAKE_DIR: u64 = 1 << 7;
    const FS_MAKE_REG: u64 = 1 << 8;
    const FS_MAKE_SOCK: u64 = 1 << 9;
    const FS_MAKE_FIFO: u64 = 1 << 10;
    const FS_MAKE_BLOCK: u64 = 1 << 11;
    const FS_MAKE_SYM: u64 = 1 << 12;
    const FS_REFER: u64 = 1 << 13; // v2
    const FS_TRUNCATE: u64 = 1 << 14; // v3

    const FS_RO: u64 = FS_EXECUTE | FS_READ_FILE | FS_READ_DIR;
    const FS_RW_V1: u64 = FS_RO
        | FS_WRITE_FILE
        | FS_REMOVE_DIR
        | FS_REMOVE_FILE
        | FS_MAKE_CHAR
        | FS_MAKE_DIR
        | FS_MAKE_REG
        | FS_MAKE_SOCK
        | FS_MAKE_FIFO
        | FS_MAKE_BLOCK
        | FS_MAKE_SYM
        | FS_REFER
        | FS_TRUNCATE;

    let handled: u64 = if abi >= 3 {
        FS_RW_V1
    } else if abi == 2 {
        FS_RW_V1 & !FS_TRUNCATE
    } else {
        FS_RW_V1 & !(FS_REFER | FS_TRUNCATE)
    };
    // ABI 4: attr расширяется полем handled_access_net. Нулевое поле
    // сети = сеть не ограничивается; ненулевое = всё вне правил запрещено.
    const NET_BIND_TCP: u64 = 1 << 0;
    const NET_CONNECT_TCP: u64 = 1 << 1;
    let handled_net: u64 = if want_net {
        let mut mask = 0u64;
        if let NetPolicy::Restrict {
            allow_connect,
            allow_bind,
        } = net
        {
            if !allow_bind.is_empty() {
                mask |= NET_BIND_TCP;
            }
            if !allow_connect.is_empty() {
                mask |= NET_CONNECT_TCP;
            }
            // restrict с пустыми списками = запретить и bind, и connect
            if allow_bind.is_empty() && allow_connect.is_empty() {
                mask = NET_BIND_TCP | NET_CONNECT_TCP;
            }
        }
        mask
    } else {
        0
    };
    #[repr(C)]
    struct RulesetAttr {
        handled_access_fs: u64,
        handled_access_net: u64,
    }
    let attr = RulesetAttr {
        handled_access_fs: handled,
        handled_access_net: handled_net,
    };
    // ABI < 4 знает только первое поле: размер структуры — по возможностям.
    let attr_size = if abi >= 4 { 16usize } else { 8usize };
    let ruleset_fd = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            &attr as *const RulesetAttr,
            attr_size,
            0u32,
        )
    };
    if ruleset_fd < 0 {
        return Err(io::Error::new(
            io::Error::last_os_error().kind(),
            format!("create_ruleset: {}", io::Error::last_os_error()),
        ));
    }
    let ruleset_fd = ruleset_fd as i32;

    let result = (|| -> io::Result<()> {
        for (path, rights) in rules {
            // O_PATH БЕЗ O_NOFOLLOW: /bin на современных дистрибутивах
            // — симлинк на /usr/bin; add_rule по fd симлинка возвращает
            // EINVAL (landlock требует реальный inode).
            let fd = unsafe { libc::open(path.as_ptr(), libc::O_PATH | libc::O_CLOEXEC) };
            if fd < 0 {
                return Err(io::Error::new(
                    io::Error::last_os_error().kind(),
                    format!(
                        "open {}: {}",
                        path.to_string_lossy(),
                        io::Error::last_os_error()
                    ),
                ));
            }
            #[repr(C)]
            struct PathBeneath {
                allowed_access: u64,
                parent_fd: i32,
            }
            let rule = PathBeneath {
                allowed_access: rights & handled,
                parent_fd: fd,
            };
            // LANDLOCK_RULE_PATH_BENEATH = 1
            let rc = unsafe {
                libc::syscall(
                    libc::SYS_landlock_add_rule,
                    ruleset_fd,
                    1u32,
                    &rule as *const PathBeneath,
                    0u32,
                )
            };
            unsafe { libc::close(fd) };
            if rc < 0 {
                return Err(io::Error::new(
                    io::Error::last_os_error().kind(),
                    format!(
                        "add_rule {}: {}",
                        path.to_string_lossy(),
                        io::Error::last_os_error()
                    ),
                ));
            }
        }
        // Сетевые правила (ABI 4): LANDLOCK_RULE_NET_PORT = 2,
        // landlock_net_port_attr { allowed_access: u64, port: u64 }.
        if let NetPolicy::Restrict {
            allow_connect,
            allow_bind,
        } = net
        {
            #[repr(C)]
            struct NetPortAttr {
                allowed_access: u64,
                port: u64,
            }
            let add_net = |access: u64, port: u16| -> io::Result<()> {
                let rule = NetPortAttr {
                    allowed_access: access,
                    port: u64::from(port),
                };
                let rc = unsafe {
                    libc::syscall(
                        libc::SYS_landlock_add_rule,
                        ruleset_fd,
                        2u32,
                        &rule as *const NetPortAttr,
                        0u32,
                    )
                };
                if rc < 0 {
                    return Err(io::Error::new(
                        io::Error::last_os_error().kind(),
                        format!("add_net_rule port {port}: {}", io::Error::last_os_error()),
                    ));
                }
                Ok(())
            };
            for port in allow_bind {
                add_net(NET_BIND_TCP, *port)?;
            }
            for port in allow_connect {
                add_net(NET_CONNECT_TCP, *port)?;
            }
        }
        if unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } < 0 {
            return Err(io::Error::new(
                io::Error::last_os_error().kind(),
                format!("no_new_privs: {}", io::Error::last_os_error()),
            ));
        }
        if unsafe { libc::syscall(libc::SYS_landlock_restrict_self, ruleset_fd, 0u32) } < 0 {
            return Err(io::Error::new(
                io::Error::last_os_error().kind(),
                format!("restrict_self: {}", io::Error::last_os_error()),
            ));
        }
        Ok(())
    })();
    unsafe { libc::close(ruleset_fd) };
    result
}

#[cfg(not(target_os = "linux"))]
#[allow(dead_code)] // заглушка для симметрии API; вызывается только на Linux
pub fn confine_current_process(_rules: &[(std::ffi::CString, u64)]) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "Landlock — только Linux",
    ))
}

/// Набор правил для рабочей области: область RW, система RO, /tmp и
/// /dev RW, /proc и /sys RO (тулчейнам нужны). Пути готовятся как
/// CString ДО fork-точки (pre_exec не должен аллоцировать).
#[cfg(target_os = "linux")]
pub fn workspace_rules(workspace: &Path) -> io::Result<Vec<(std::ffi::CString, u64)>> {
    use std::os::unix::ffi::OsStrExt;
    const FS_RO: u64 = (1 << 0) | (1 << 2) | (1 << 3);
    const FS_RW: u64 = (1 << 0)
        | (1 << 1)
        | (1 << 2)
        | (1 << 3)
        | (1 << 4)
        | (1 << 5)
        | (1 << 6)
        | (1 << 7)
        | (1 << 8)
        | (1 << 9)
        | (1 << 10)
        | (1 << 11)
        | (1 << 12)
        | (1 << 13)
        | (1 << 14);
    let mut rules: Vec<(std::ffi::CString, u64)> = Vec::new();
    let mut push = |path: &Path, rights: u64| -> io::Result<()> {
        if path.exists() {
            let c = std::ffi::CString::new(path.as_os_str().as_bytes())
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "NUL в пути"))?;
            rules.push((c, rights));
        }
        Ok(())
    };
    push(workspace, FS_RW)?;
    for ro in [
        "/usr", "/bin", "/sbin", "/lib", "/lib64", "/etc", "/opt", "/proc", "/sys",
    ] {
        push(Path::new(ro), FS_RO)?;
    }
    for rw in ["/tmp", "/dev", "/var/tmp"] {
        push(Path::new(rw), FS_RW)?;
    }
    // Домашние инструментальные каталоги пользователя (тулчейны живут
    // там: ~/.cargo, ~/.rustup, ~/.local, ~/.nvm) — RO: команда может
    // вызывать cargo/node, но не переписать их.
    if let Some(home) = std::env::var_os("HOME") {
        for sub in [".cargo", ".rustup", ".local", ".nvm", ".npm", ".cache"] {
            push(&Path::new(&home).join(sub), FS_RO)?;
        }
    }
    Ok(rules)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_parse_roundtrip() {
        assert_eq!(LandlockMode::parse("auto").unwrap(), LandlockMode::Auto);
        assert_eq!(
            LandlockMode::parse("require").unwrap(),
            LandlockMode::Require
        );
        assert!(LandlockMode::parse("sometimes").is_err());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn confined_network_restrict_blocks_unlisted_port() {
        if kernel_abi() < 4 {
            return; // ядро старше 6.7 — пропуск, не провал
        }
        use std::os::unix::process::CommandExt;
        use std::process::Command;
        // Разрешён только connect на 443: попытка соединения с портом 9
        // (discard) из замкнутого процесса обязана быть отвергнута ядром
        // (EACCES), а не отказом удалённой стороны.
        let mut probe = Command::new("sh");
        probe.arg("-c").arg("exec 3<>/dev/tcp/127.0.0.1/9");
        let rules = workspace_rules(std::path::Path::new("/tmp")).expect("rules");
        let net = NetPolicy::Restrict {
            allow_connect: vec![443],
            allow_bind: vec![],
        };
        unsafe {
            probe.pre_exec(move || confine_current_process_net(&rules, &net));
        }
        let output = probe.output().expect("spawn");
        assert!(
            !output.status.success(),
            "порт вне списка должен быть отвергнут песочницей"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn debug_confine_stage() {
        if !kernel_supports() {
            return;
        }
        let dir = std::env::temp_dir().join(format!("berimor-lld-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let rules = workspace_rules(&dir).unwrap();
        // fork вручную: ребёнок замыкается и _exit(0), родитель читает
        // код — точная стадия видна по коду ошибки, процесс тестов не
        // замыкается (confine в родителе был бы необратим).
        let pid = unsafe { libc::fork() };
        if pid == 0 {
            match confine_current_process(&rules) {
                Ok(()) => unsafe { libc::_exit(0) },
                Err(e) => {
                    let msg = format!("{e}\n");
                    unsafe {
                        libc::write(2, msg.as_ptr() as *const libc::c_void, msg.len());
                        libc::_exit(1)
                    }
                }
            }
        }
        let mut status = 0;
        unsafe { libc::waitpid(pid, &mut status, 0) };
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(libc::WEXITSTATUS(status), 0, "стадия confine");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn confined_child_cannot_write_outside_workspace() {
        if !kernel_supports() {
            return; // CI без Landlock — пропуск, не провал
        }
        let dir = std::env::temp_dir().join(format!("berimor-ll-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let rules = workspace_rules(&dir).unwrap();
        // Внутри области — можно.
        let mut inside = std::process::Command::new("sh");
        inside
            .arg("-c")
            .arg("touch ok.txt && echo ok")
            .current_dir(&dir);
        unsafe {
            use std::os::unix::process::CommandExt;
            let rules = rules.clone();
            inside.pre_exec(move || confine_current_process(&rules));
        }
        let out = inside.output().unwrap();
        assert!(out.status.success(), "внутри области: {:?}", out);
        // Вне области — нельзя (запись в $HOME запрещена).
        let mut outside = std::process::Command::new("sh");
        outside
            .arg("-c")
            .arg("echo x > \"$HOME/.berimor-landlock-probe\"");
        unsafe {
            use std::os::unix::process::CommandExt;
            let rules = rules.clone();
            outside.pre_exec(move || confine_current_process(&rules));
        }
        let out = outside.output().unwrap();
        assert!(!out.status.success(), "запись вне области должна падать");
        let home_probe = std::path::PathBuf::from(std::env::var_os("HOME").unwrap())
            .join(".berimor-landlock-probe");
        assert!(!home_probe.exists(), "файл не должен был создаться");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
