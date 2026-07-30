//! Анализатор deny-статики: таблица правил по запрещённым классам операций.
//!
//! Источник: `docs/arch/security-model.md` §1 (модель угроз), §2 (слой L3),
//! `docs/arch/ideal-agent-architecture.md` §3.7 п.1 (перечень классов),
//! ADR-0007 (deny безусловен — подтверждение человека его не отменяет, I6).
//! ROADMAP: S1.
//!
//! Анализатор работает ДО выполнения, по тексту предложенного действия, и не
//! полагается ни на какую модель. Пять запрещённых классов — дословно из
//! §3.7 п.1: разрушение файловых систем, запись на блочные устройства,
//! эскалация привилегий, fork-бомбы, удаление вне рабочей области.
//!
//! Границы слоя (честно, для ревью):
//! - анализируются строки под объявленными ключами команд
//!   ([`COMMAND_KEYS`]) и путей ([`PATH_KEYS`]) — канал декларируется типом
//!   инструмента, не угадывается из произвольного текста («анализ всех целей
//!   одной операции», security-model.md §1);
//! - проверка путей здесь — текстовая (лексическая). Структурная защита от
//!   symlink-обхода — слой jail (S2, `jail.rs`), который вызывается самим
//!   инструментом при реальном обращении к ФС; deny-статика не заменяет jail,
//!   как и наоборот (эшелонированная оборона, ADR-0007);
//! - подстановки окружения (`$VAR`) не разрешаются — анализатор не
//!   интерпретирует shell. Цель рекурсивного удаления, которую нельзя
//!   доказуемо отнести внутрь рабочей области, блокируется (консервативный
//!   выбор: «не могу доказать безопасность» = deny).

use berimor_types::capability::ProposedAction;
use serde_json::Value;
use std::path::{Component, Path, PathBuf};

/// Ключи аргументов, чьи строковые значения трактуются как текст команды.
pub const COMMAND_KEYS: &[&str] = &["command", "cmd", "script", "shell", "run"];

/// Ключи аргументов, чьи строковые значения трактуются как пути в ФС.
pub const PATH_KEYS: &[&str] = &[
    "path",
    "file",
    "dir",
    "directory",
    "target",
    "destination",
    "dest",
];

/// Запрещённый класс операции (§3.7 п.1). Имена вариантов совпадают со
/// строками `class` в golden-фикстуре
/// `fixtures/golden/security/denied-operations.json` посимвольно —
/// контрактный тест сравнивает их буквально.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForbiddenClass {
    /// Разрушение файловых систем (форматирование, стирание, `rm -rf /`).
    FilesystemDestruction,
    /// Запись на блочные устройства (`dd of=/dev/sd*`, `> /dev/nvme*`).
    BlockDeviceWrite,
    /// Эскалация привилегий (`sudo`, setuid, `chown root`).
    PrivilegeEscalation,
    /// Fork-бомба.
    ForkBomb,
    /// Удаление/модификация вне рабочей области (включая недоказуемые цели).
    DeletionOutsideWorkspace,
}

impl ForbiddenClass {
    /// Имя класса как в фикстуре — единственное представление, чтобы тест
    /// не держал свою таблицу соответствия, которая может разъехаться.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::FilesystemDestruction => "filesystem_destruction",
            Self::BlockDeviceWrite => "block_device_write",
            Self::PrivilegeEscalation => "privilege_escalation",
            Self::ForkBomb => "fork_bomb",
            Self::DeletionOutsideWorkspace => "deletion_outside_workspace",
        }
    }
}

/// Срабатывание deny-статики: класс + фрагмент, на котором сработало правило
/// (для журнала и текста отказа — security-model.md §5 требует, чтобы
/// события безопасности были первоклассными и объяснимыми).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DenyMatch {
    pub class: ForbiddenClass,
    pub evidence: String,
}

/// Анализирует предложенное действие по deny-таблице. `Some` — безусловный
/// запрет; `None` — deny-статика не против (подтверждение по режиму —
/// следующий слой, S4). `workspace_root` — канонический корень рабочей
/// области; относительные цели отсчитываются от него.
pub fn analyze(action: &ProposedAction, workspace_root: &Path) -> Option<DenyMatch> {
    for (key, text) in collect_strings(&action.args) {
        if COMMAND_KEYS.contains(&key.as_str()) {
            if let Some(m) = analyze_command(&text, workspace_root) {
                return Some(m);
            }
        }
        // Текстовая проверка выхода за рабочую область — только для
        // мутирующих действий: чтение вне области не входит в перечень
        // запрещённых классов §3.7 (см. allowed-кейсы фикстуры).
        if action.mutates
            && PATH_KEYS.contains(&key.as_str())
            && !path_within(&text, workspace_root)
        {
            return Some(DenyMatch {
                class: ForbiddenClass::DeletionOutsideWorkspace,
                evidence: text,
            });
        }
    }
    None
}

/// Рекурсивно собирает (путь-ключа, строка) из аргументов: вложенные
/// объекты/массивы не должны быть способом спрятать команду от анализа.
fn collect_strings(value: &Value) -> Vec<(String, String)> {
    let mut out = Vec::new();
    fn walk(key: &str, value: &Value, out: &mut Vec<(String, String)>) {
        match value {
            Value::String(s) => out.push((key.to_string(), s.clone())),
            Value::Object(map) => {
                for (k, v) in map {
                    walk(k, v, out);
                }
            }
            Value::Array(items) => {
                for item in items {
                    walk(key, item, out);
                }
            }
            _ => {}
        }
    }
    walk("", value, &mut out);
    out
}

/// Анализ одного текста команды: цепочки (`&&`, `||`, `;`, переводы строк)
/// и подоболочки (`$(...)`, обратные кавычки) разбираются рекурсивно —
/// security-model.md §1: «обход через … цепочки команд».
fn analyze_command(text: &str, workspace_root: &Path) -> Option<DenyMatch> {
    if let Some(m) = detect_fork_bomb(text) {
        return Some(m);
    }
    for segment in split_chain(text) {
        let tokens = tokenize(&segment);
        if tokens.is_empty() {
            continue;
        }
        if let Some(m) = detect_device_redirect(&tokens) {
            return Some(m);
        }
        let program = basename(&tokens[0]);
        let args = &tokens[1..];
        let m = match program.as_str() {
            p if p.starts_with("mkfs") || p == "wipefs" || p == "fdisk" || p == "parted" => {
                Some((ForbiddenClass::FilesystemDestruction, segment.clone()))
            }
            "shred" if args.iter().any(|a| is_block_device(a)) => {
                Some((ForbiddenClass::FilesystemDestruction, segment.clone()))
            }
            "dd" => args.iter().find_map(|a| {
                a.strip_prefix("of=")
                    .filter(|target| is_block_device(target))
                    .map(|target| (ForbiddenClass::BlockDeviceWrite, target.to_string()))
            }),
            "rm" => analyze_rm(args, workspace_root).map(|m| (m.class, m.evidence)),
            "sudo" | "doas" | "su" => Some((ForbiddenClass::PrivilegeEscalation, segment.clone())),
            "chmod" if is_setuid_chmod(args) => {
                Some((ForbiddenClass::PrivilegeEscalation, segment.clone()))
            }
            "chown" | "chgrp" if args.iter().any(|a| a == "root" || a.starts_with("root:")) => {
                Some((ForbiddenClass::PrivilegeEscalation, segment.clone()))
            }
            _ => None,
        };
        if let Some((class, evidence)) = m {
            return Some(DenyMatch { class, evidence });
        }
        // Подоболочки внутри сегмента — рекурсивно, теми же правилами.
        for subshell in extract_subshells(&segment) {
            if let Some(m) = analyze_command(&subshell, workspace_root) {
                return Some(m);
            }
        }
    }
    None
}

/// Разбивает текст на отдельные команды цепочки.
fn split_chain(text: &str) -> Vec<String> {
    text.split(['&', '|', ';', '\n'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// Достаёт содержимое `$(...)` и обратных кавычек (без учёта вложенности
/// сверх первого уровня — содержимое уходит в рекурсивный вызов, который
/// разберёт вложенность сам).
fn extract_subshells(segment: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = segment;
    while let Some(start) = rest.find("$(") {
        let after = &rest[start + 2..];
        match after.find(')') {
            Some(end) => {
                out.push(after[..end].to_string());
                rest = &after[end + 1..];
            }
            None => break,
        }
    }
    let mut rest = segment;
    while let Some(start) = rest.find('`') {
        let after = &rest[start + 1..];
        match after.find('`') {
            Some(end) => {
                out.push(after[..end].to_string());
                rest = &after[end + 1..];
            }
            None => break,
        }
    }
    out
}

fn tokenize(segment: &str) -> Vec<String> {
    segment
        .split_whitespace()
        .map(|t| t.trim_matches(|c| c == '"' || c == '\'').to_string())
        .filter(|t| !t.is_empty())
        .collect()
}

fn basename(program: &str) -> String {
    program.rsplit('/').next().unwrap_or(program).to_string()
}

/// Перенаправление вывода на блочное устройство: токен `>`/`>>` и рядом
/// путь устройства, либо слитная форма `>/dev/sda`.
fn detect_device_redirect(tokens: &[String]) -> Option<DenyMatch> {
    for (i, token) in tokens.iter().enumerate() {
        let target = if token == ">" || token == ">>" {
            tokens.get(i + 1).map(String::as_str)
        } else {
            token
                .trim_start_matches(|c: char| c.is_ascii_digit() || c == '&')
                .strip_prefix('>')
                .map(|rest| rest.trim_start_matches('>'))
        };
        if let Some(target) = target {
            if is_block_device(target) {
                return Some(DenyMatch {
                    class: ForbiddenClass::BlockDeviceWrite,
                    evidence: target.to_string(),
                });
            }
        }
    }
    None
}

/// Блочное устройство по шаблону имени: sd/hd/vd/xvd, nvme, mmcblk, loop,
/// device-mapper. `/dev/null`, `/dev/stdout` и т.п. сознательно НЕ
/// блокируются (allowed-кейс фикстуры) — они не хранят данные ФС.
fn is_block_device(path: &str) -> bool {
    let Some(name) = path.strip_prefix("/dev/") else {
        return false;
    };
    ["sd", "hd", "vd", "xvd", "nvme", "mmcblk", "loop", "dm-"]
        .iter()
        .any(|prefix| name.starts_with(prefix))
}

/// `rm` с рекурсивным флагом по каждой цели: корень/glob — разрушение ФС;
/// цель вне рабочей области или недоказуемая (`~`, `$VAR`) — удаление вне
/// области. Без рекурсивного флага `rm` deny-статикой не блокируется
/// (единичное удаление внутри области — обычная операция, её место в
/// режимах подтверждений S4).
fn analyze_rm(args: &[String], workspace_root: &Path) -> Option<DenyMatch> {
    let recursive = args.iter().any(|a| {
        a.starts_with('-') && !a.starts_with("--") && a.contains(['r', 'R']) || a == "--recursive"
    });
    if !recursive {
        return None;
    }
    for target in args.iter().filter(|a| !a.starts_with('-')) {
        if target == "/" || target == "/*" {
            return Some(DenyMatch {
                class: ForbiddenClass::FilesystemDestruction,
                evidence: target.clone(),
            });
        }
        if !path_within(target, workspace_root) {
            return Some(DenyMatch {
                class: ForbiddenClass::DeletionOutsideWorkspace,
                evidence: target.clone(),
            });
        }
    }
    None
}

/// Можно ли доказать, что цель остаётся внутри рабочей области. `~` и
/// `$VAR` не разворачиваются — недоказуемо, значит вне (консервативно).
/// Лексическая проверка; symlink-обход — забота jail (S2).
fn path_within(target: &str, workspace_root: &Path) -> bool {
    if target.contains('~') || target.contains('$') {
        return false;
    }
    let candidate = if Path::new(target).is_absolute() {
        PathBuf::from(target)
    } else {
        workspace_root.join(target)
    };
    normalize_lexically(&candidate).starts_with(workspace_root)
}

/// Лексическая нормализация: `.` убираются, `..` схлопываются без обращения
/// к ФС. Дубликат `std::path::absolute` не используем — тот не убирает `..`.
fn normalize_lexically(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// setuid/setgid через символический (`u+s`, `g+s`, `+s`) или числовой
/// (старшая цифра 4/6/2 — setuid/setgid-биты) режим.
fn is_setuid_chmod(args: &[String]) -> bool {
    args.iter().any(|a| {
        let a = a.trim_start_matches('-');
        (a.contains("u+s") || a.contains("g+s") || a.starts_with("+s"))
            || (a.len() == 4
                && a.chars().all(|c| c.is_ascii_digit())
                && matches!(a.chars().next(), Some('2' | '4' | '6')))
    })
}

/// Fork-бомба: объявление функции `name(){`, чьё тело вызывает себя через
/// конвейер с фоновым запуском. Нормализация — удаление пробелов:
/// `:(){ :|:& };:` и `:(){:|:&};:` — одна и та же бомба.
fn detect_fork_bomb(text: &str) -> Option<DenyMatch> {
    let compact: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    if let Some(pos) = compact.find("(){") {
        let body = &compact[pos..];
        if body.contains('|') && body.contains('&') && body.contains('}') {
            return Some(DenyMatch {
                class: ForbiddenClass::ForkBomb,
                evidence: text.trim().to_string(),
            });
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::PathBuf;

    const FIXTURE: &str = include_str!("../../../fixtures/golden/security/denied-operations.json");

    #[derive(serde::Deserialize)]
    struct Fixture {
        workspace_root: String,
        denied: Vec<FixtureCase>,
        allowed: Vec<FixtureCase>,
    }

    #[derive(serde::Deserialize)]
    struct FixtureCase {
        name: String,
        tool: String,
        args: Value,
        class: Option<String>,
    }

    fn action(case: &FixtureCase) -> ProposedAction {
        ProposedAction {
            tool: case.tool.clone(),
            args: case.args.clone(),
            // Фикстура описывает предложенные операции; проверка путей
            // слоя S1 относится к мутирующим действиям — прогоняем все
            // кейсы как мутирующие, текст решает остальное.
            mutates: true,
        }
    }

    /// Контрактный тест DoD Фазы 4 (quality-attributes.md, строка
    /// «Безопасность (деструктив)»): deny-таблица блокирует ВЕСЬ перечень
    /// запрещённых операций из золотого набора, и каждую — ожидаемым классом.
    #[test]
    fn golden_denied_operations_are_all_blocked_with_expected_class() {
        let fixture: Fixture = serde_json::from_str(FIXTURE).unwrap();
        let root = PathBuf::from(&fixture.workspace_root);
        assert!(
            !fixture.denied.is_empty(),
            "фикстура без denied-кейсов пуста"
        );

        for case in &fixture.denied {
            let verdict = analyze(&action(case), &root);
            let m = verdict
                .unwrap_or_else(|| panic!("'{}' обязана блокироваться deny-статикой", case.name));
            assert_eq!(
                Some(m.class.as_str()),
                case.class.as_deref(),
                "кейс '{}' заблокирован не тем классом",
                case.name
            );
        }
    }

    /// Симметричная половина контракта: обычные операции не должны
    /// блокироваться — deny-статика, ловящая всё подряд, делает систему
    /// непригодной (и маскирует ошибки таблицы).
    #[test]
    fn golden_allowed_operations_are_not_blocked() {
        let fixture: Fixture = serde_json::from_str(FIXTURE).unwrap();
        let root = PathBuf::from(&fixture.workspace_root);
        assert!(
            !fixture.allowed.is_empty(),
            "фикстура без allowed-кейсов пуста"
        );

        for case in &fixture.allowed {
            assert!(
                analyze(&action(case), &root).is_none(),
                "'{}' не должна блокироваться deny-статикой",
                case.name
            );
        }
    }

    #[test]
    fn non_mutating_path_outside_workspace_is_not_denied() {
        // Чтение вне области — не из запрещённых классов §3.7.
        let action = ProposedAction {
            tool: "files.read".into(),
            args: json!({"path": "/etc/hostname"}),
            mutates: false,
        };
        assert!(analyze(&action, Path::new("/workspace")).is_none());
    }

    #[test]
    fn nested_command_string_is_still_analyzed() {
        let action = ProposedAction {
            tool: "terminal".into(),
            args: json!({"steps": [{"command": "rm -rf /"}]}),
            mutates: true,
        };
        let m = analyze(&action, Path::new("/workspace")).unwrap();
        assert_eq!(m.class, ForbiddenClass::FilesystemDestruction);
    }

    #[test]
    fn rm_without_recursive_flag_is_left_to_confirmation_layer() {
        let action = ProposedAction {
            tool: "terminal".into(),
            args: json!({"command": "rm /workspace/tmp/file"}),
            mutates: true,
        };
        assert!(analyze(&action, Path::new("/workspace")).is_none());
    }

    #[test]
    fn evidence_points_at_the_matching_fragment() {
        let action = ProposedAction {
            tool: "terminal".into(),
            args: json!({"command": "ls && dd of=/dev/sda"}),
            mutates: true,
        };
        let m = analyze(&action, Path::new("/workspace")).unwrap();
        assert_eq!(m.class, ForbiddenClass::BlockDeviceWrite);
        assert!(m.evidence.contains("/dev/sda"));
    }
}
