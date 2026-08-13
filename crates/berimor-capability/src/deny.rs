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
//! эскалация привилегий, fork-бомбы, удаление/модификация вне рабочей области.
//!
//! После независимого XL-ревью S1–S4 анализатор дополнен против конкретных,
//! проверенных кодом обходов: обёртки-интерпретаторы (`bash -c`, `env`,
//! `nice`, `timeout`, `xargs`, префиксы `VAR=...`) разворачиваются
//! рекурсивно; пути устройств нормализуются (`/dev/./sda`, `/dev//sda`,
//! `/dev/mapper/*`, `/dev/disk/*`); `find -delete` обрабатывается наравне с
//! `rm`; токенизатор понимает shell-кавычки и экранирование; подоболочки
//! `$(...)` разбираются с подсчётом глубины; недоказуемые цели записи
//! (`$VAR`) блокируются консервативно.
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
//! - скрипты, запускаемые интерпретатором файлом (`bash exploit.sh`),
//!   не анализируются — содержимое файла недоступно статическому анализу
//!   команды; закрывается тем, что запись файла вне области — отдельный
//!   запрет, а исполнение внутри области ограничено jail/режимами;
//! - генерация fork-бомб на языках интерпретаторов (`perl -e 'fork while
//!   fork'`) — вне досягаемости текстового анализа программ-обёрток;
//!   детектируется shell-форма объявления функции с самовызовом.

use berimor_types::capability::ProposedAction;
use serde_json::Value;
use std::path::{Component, Path, PathBuf};

/// Ключи аргументов, чьи строковые значения трактуются как текст команды.
pub const COMMAND_KEYS: &[&str] = &["command", "cmd", "script", "shell", "run"];

/// Совпадение ключа аргумента с соглашением — РЕГИСТРОНЕЗАВИСИМО
/// (находка 2.19 аудита: `{"Command": "rm -rf /"}` проходил мимо
/// регистрозависимого разбора; ключ `Command`/`Path` у инструмента —
/// та же семантика, молчаливый пропуск недопустим).
pub(crate) fn key_matches(key: &str, keys: &[&str]) -> bool {
    keys.iter().any(|k| k.eq_ignore_ascii_case(key))
}

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

/// Символьные устройства-пустышки: запись в них не разрушает данные.
const HARMLESS_CHAR_DEVICES: &[&str] = &[
    "/dev/null",
    "/dev/zero",
    "/dev/full",
    "/dev/stdout",
    "/dev/stderr",
    "/dev/tty",
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

/// Контрольные файлы гейта (XL-ревью 2026-08-13 HIGH #2): allow-лист
/// (`*` = авто-подтверждение ВСЕХ мутаций) — обычный файл внутри
/// workspace; однажды подтверждённый files.write мог молча выписать
/// агенту `*` — персистентная эскалация. Запись в контрольные файлы
/// запрещена безусловно (чтение — свободно, секретов там нет).
const CONTROL_FILES: &[&str] = &[".berimor/allow", ".berimor-allow"];

/// Цель PATH_KEYS — контрольный файл гейта? Относительные цели — от
/// корня области; `./`-префикс не должен обходить правило.
fn is_control_path(text: &str, workspace_root: &Path) -> bool {
    let trimmed = text.trim().trim_start_matches("./");
    let path = Path::new(trimmed);
    let rel = path.strip_prefix(workspace_root).unwrap_or(path);
    CONTROL_FILES.iter().any(|name| rel == Path::new(name))
}

/// Команда пишет в контрольный файл? Только с оператором записи рядом:
/// `cat .berimor/allow` — легитимное чтение, `echo '*' > .berimor/allow`
/// — самоназначение разрешений. Подстроки — тот же приём, что и в
/// остальной deny-таблице (ложные срабатывания дешевле пропусков).
fn analyze_control_file_write(text: &str) -> Option<DenyMatch> {
    const WRITE_OPS: &[&str] = &[
        ">", "tee", "sed -i", "dd ", "truncate", "install ", "cp ", "mv ", "rsync",
    ];
    for name in CONTROL_FILES {
        if text.contains(name) && WRITE_OPS.iter().any(|op| text.contains(op)) {
            return Some(DenyMatch {
                class: ForbiddenClass::PrivilegeEscalation,
                evidence: text.chars().take(120).collect(),
            });
        }
    }
    None
}

/// Анализирует предложенное действие по deny-таблице. `Some` — безусловный
/// запрет; `None` — deny-статика не против (подтверждение по режиму —
/// следующий слой, S4). `workspace_root` — канонический корень рабочей
/// области; относительные цели отсчитываются от него.
pub fn analyze(action: &ProposedAction, workspace_root: &Path) -> Option<DenyMatch> {
    // Команды — формо-зависимо (аудит 2.9): текст разбирается по
    // shell-семантике (цепочки, подоболочки), argv-массив — по
    // execve-семантике (элемент = токен, без цепочек); раньше массив
    // распадался на безобидные поодиночке элементы.
    let mut commands = Vec::new();
    collect_commands(&action.args, &mut commands);
    for form in &commands {
        if let CommandForm::Text(text) = form {
            // Запись в контрольные файлы гейта — до общего разбора:
            // безусловный запрет независимо от формы команды.
            if let Some(m) = analyze_control_file_write(text) {
                return Some(m);
            }
        }
        let found = match form {
            CommandForm::Text(text) => analyze_command(text, workspace_root),
            CommandForm::Argv(items) => analyze_argv(items, workspace_root),
        };
        if let Some(m) = found {
            return Some(m);
        }
    }
    for (key, text) in collect_strings(&action.args) {
        // Текстовая проверка выхода за рабочую область — для мутирующих
        // действий: чтение вне области не входит в перечень запрещённых
        // классов §3.7. Флаг `mutates` вызывающего кода дополняется
        // эвристикой имени инструмента — флаг мог быть неизвестен
        // вызывающему (находка m10 XL-ревью).
        let mutating = action.mutates || looks_mutating(&action.tool);
        if mutating && key_matches(&key, PATH_KEYS) {
            if is_control_path(&text, workspace_root) {
                return Some(DenyMatch {
                    class: ForbiddenClass::PrivilegeEscalation,
                    evidence: text,
                });
            }
            if !path_within(&text, workspace_root) {
                return Some(DenyMatch {
                    class: ForbiddenClass::DeletionOutsideWorkspace,
                    evidence: text,
                });
            }
        }
    }
    None
}

/// Имя инструмента, намекающее на мутацию, — подстраховка к флагу
/// `ProposedAction.mutates`, который вызывающий код может не знать.
fn looks_mutating(tool: &str) -> bool {
    const MARKERS: &[&str] = &[
        "delete", "remove", "write", "move", "copy", "rename", "create", "update", "set", "put",
    ];
    MARKERS.iter().any(|marker| tool.contains(marker))
}

/// Рекурсивно собирает (путь-ключа, строка) из аргументов: вложенные
/// объекты/массивы не должны быть способом спрятать команду от анализа.
/// `pub(crate)` — тот же обход нужен jail-слою в `confirm.rs` (S2).
pub(crate) fn collect_strings(value: &Value) -> Vec<(String, String)> {
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
        // Подоболочки сегмента — первыми: `echo $(sudo whoami)` обязано
        // сработать даже без запретов в самой внешней команде.
        for subshell in extract_subshells(&segment) {
            if let Some(m) = analyze_command(&subshell, workspace_root) {
                return Some(m);
            }
        }
        if let Some(m) = analyze_segment(&segment, workspace_root) {
            return Some(m);
        }
    }
    None
}

/// Форма команды в аргументах действия (аудит 2.9).
enum CommandForm {
    /// Текст для shell-интерпретации: цепочки и подоболочки значимы.
    Text(String),
    /// argv для прямого execve: элемент — токен, shell-разбора нет, но
    /// обёртки-интерпретаторы значимы и разворачиваются как обычно.
    Argv(Vec<String>),
}

/// Собирает команды из аргументов с сохранением формы (текст/argv).
/// Ключи — `COMMAND_KEYS`; массив под таким ключом — argv, не набор
/// независимых строк.
fn collect_commands(value: &Value, out: &mut Vec<CommandForm>) {
    fn walk(key: &str, value: &Value, out: &mut Vec<CommandForm>) {
        match value {
            Value::String(s) if key_matches(key, COMMAND_KEYS) => {
                out.push(CommandForm::Text(s.clone()));
            }
            Value::Array(items) if key_matches(key, COMMAND_KEYS) => {
                let argv: Vec<String> = items
                    .iter()
                    .filter_map(|item| item.as_str().map(String::from))
                    .collect();
                if !argv.is_empty() {
                    out.push(CommandForm::Argv(argv));
                }
            }
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
    walk("", value, out);
}

/// Анализ argv-формы: те же обёртки и правила классов, что у текстовой
/// формы, но без shell-цепочек и fork-бомб-текста (в execve-семантике
/// это литеральные аргументы; `:(){:|:&};:` как argv — несуществующее
/// имя программы, не бомба). `bash -c` внутри argv разворачивается
/// общим путём и анализируется как текст рекурсивно.
fn analyze_argv(items: &[String], workspace_root: &Path) -> Option<DenyMatch> {
    if items.is_empty() {
        return None;
    }
    analyze_tokens(items.to_vec(), &items.join(" "), workspace_root)
}

/// Анализ одного сегмента цепочки: разворачивание обёрток → программа →
/// правила классов.
fn analyze_segment(segment: &str, workspace_root: &Path) -> Option<DenyMatch> {
    let tokens = tokenize(segment);
    if tokens.is_empty() {
        return None;
    }
    analyze_tokens(tokens, segment, workspace_root)
}

/// Общая часть разбора (текстовой и argv-формы): обёртки, редиректы,
/// правила классов. `evidence` — фрагмент для журнала/текста отказа.
fn analyze_tokens(tokens: Vec<String>, evidence: &str, workspace_root: &Path) -> Option<DenyMatch> {
    let (program, args) = match unwrap_wrappers(tokens, workspace_root) {
        Some(Unwrapped::Command { program, args }) => (program, args),
        Some(Unwrapped::Denied(m)) => return Some(m),
        None => return None,
    };

    if let Some(m) = detect_write_redirects(&args, workspace_root) {
        return Some(m);
    }

    let m = match program.as_str() {
        p if is_filesystem_tool(p, &args) => {
            Some((ForbiddenClass::FilesystemDestruction, evidence.to_string()))
        }
        "shred" => args.iter().find_map(|a| {
            if is_block_device(a) {
                Some((ForbiddenClass::FilesystemDestruction, a.clone()))
            } else if !path_within(a, workspace_root) && !is_harmless_device(a) {
                Some((ForbiddenClass::DeletionOutsideWorkspace, a.clone()))
            } else {
                None
            }
        }),
        "dd" => analyze_dd(&args),
        "rm" => analyze_rm(&args, workspace_root).map(|m| (m.class, m.evidence)),
        "find" => analyze_find(&args, workspace_root).map(|m| (m.class, m.evidence)),
        // Техдолг TD2.3: `pkexec`/`systemd-run`/`setsid` — запускальщики
        // вне текущего процесса/сессии, безусловно эскалация по имени
        // (тот же класс, что `sudo`/`doas`/`su`). `setpriv`/`unshare` —
        // многоцелевые: опасны только с флагом смены UID/root-namespace,
        // не любым вызовом (`unshare -n` — сетевой namespace, не
        // привилегии) — отдельный предикат `changes_privilege_scope`.
        "sudo" | "doas" | "su" | "pkexec" | "systemd-run" | "setsid" => {
            Some((ForbiddenClass::PrivilegeEscalation, evidence.to_string()))
        }
        "setpriv" | "unshare" if changes_privilege_scope(&args) => {
            Some((ForbiddenClass::PrivilegeEscalation, evidence.to_string()))
        }
        "chmod" if is_setuid_chmod(&args) => {
            Some((ForbiddenClass::PrivilegeEscalation, evidence.to_string()))
        }
        // --reference=RFILE берёт владельца/режим из файла-образца:
        // содержимое RFILE недоказуемо статически — отказ (2.17 аудита).
        "chown" | "chgrp" | "chmod" if args.iter().any(|a| a.starts_with("--reference")) => {
            Some((ForbiddenClass::PrivilegeEscalation, evidence.to_string()))
        }
        "chown" | "chgrp" if args.iter().any(|a| is_root_owner(a)) => {
            Some((ForbiddenClass::PrivilegeEscalation, evidence.to_string()))
        }
        "tee" => analyze_write_targets(&args, workspace_root),
        "cp" | "mv" | "install" => analyze_copy(&args, workspace_root),
        // Аудит 2.12: альтернативные программы записи вне области.
        // rsync/scp — перенос: запись по цели всегда, read-only формы нет.
        "rsync" | "scp" => analyze_transfer(&args, workspace_root),
        // sed пишет в файлы-аргументы только в in-place форме.
        "sed" if is_in_place_sed(&args) => analyze_sed_in_place(&args, workspace_root),
        // tar пишет относительно -C (извлечение) и в -f (создание).
        "tar" => analyze_tar(&args, workspace_root),
        _ => None,
    };
    m.map(|(class, evidence)| DenyMatch { class, evidence })
}

/// Результат разворачивания обёрток.
enum Unwrapped {
    Command { program: String, args: Vec<String> },
    Denied(DenyMatch),
}

/// Разворачивает обёртки-интерпретаторы (находка C1 XL-ревью: вся таблица
/// обходилась одной конструкцией `bash -c "…"`). `sh -c '<команда>'`
/// рекурсивно анализируется как самостоятельный текст команды; глубина
/// разворачивания ограничена — защита от бесконечного цикла на вырожденном
/// вводе.
fn unwrap_wrappers(mut tokens: Vec<String>, workspace_root: &Path) -> Option<Unwrapped> {
    const MAX_DEPTH: usize = 8;
    for _ in 0..MAX_DEPTH {
        // Префикс-присваивания `VAR=value` перед командой.
        while tokens.first().is_some_and(|t| is_assignment(t)) {
            tokens.remove(0);
        }
        let program = basename(tokens.first()?);
        let rest: Vec<String> = tokens[1..].to_vec();
        match program.as_str() {
            "sh" | "bash" | "dash" | "zsh" | "ksh" => {
                if let Some(pos) = rest.iter().position(|t| t == "-c") {
                    if let Some(cmd) = rest.get(pos + 1) {
                        if let Some(m) = analyze_command(cmd, workspace_root) {
                            return Some(Unwrapped::Denied(m));
                        }
                    }
                }
                // `bash script.sh` — содержимое файла статически не
                // анализируется (см. границы слоя в шапке модуля).
                return Some(Unwrapped::Command {
                    program,
                    args: rest,
                });
            }
            "env" => {
                // Техдолг TD2.1: `env -u X <команда>` — `-u`/`-C`/`-S`/`-a`
                // /`-P` (GNU coreutils) принимают ОТДЕЛЬНОЕ значение;
                // прежний `skip_while(starts_with('-') || is_assignment)`
                // пропускал только сам флаг, а следующий токен (значение)
                // становился "программой" на следующей итерации —
                // реальная команда уходила необработанной в args.
                // Грамматика `env`: `[OPTION]... [NAME=VALUE]... [COMMAND
                // [ARG]...]` — опции и присваивания могут чередоваться,
                // поэтому не подходит общий `skip_options` (тот не знает
                // про присваивания) — свой маленький цикл, различающий
                // «флаг без значения», «флаг со значением» и
                // «присваивание».
                let mut rest = rest;
                while let Some(first) = rest.first() {
                    if is_assignment(first) {
                        rest.remove(0);
                        continue;
                    }
                    if first.starts_with('-') {
                        let takes_value = ["-u", "-C", "-S", "-a", "-P"].contains(&first.as_str());
                        rest.remove(0);
                        if takes_value && !rest.is_empty() {
                            rest.remove(0);
                        }
                        continue;
                    }
                    break;
                }
                tokens = rest;
            }
            // Техдолг TD2.2: bash-билтины, "прозрачно" исполняющие
            // следующую команду — не внешние программы со своим стилем
            // опций (в отличие от `env`/`nice`/`timeout` выше), просто
            // съедают собственное имя и передают остаток как есть на
            // следующую итерацию цикла (тот же путь, что уже даёт `sh -c`
            // для `bash script.sh` — без опций, статически не анализуется
            // глубже одного слоя).
            "exec" | "command" | "builtin" => {
                tokens = rest;
            }
            "nice" | "nohup" | "stdbuf" | "ionice" => {
                // Аудит 2.11: длинная форма `--adjustment` тоже со
                // значением — пропуск только `-n` сдвигал разбор.
                tokens = skip_options(rest, &["-n", "--adjustment"]);
            }
            "timeout" => {
                // Опции, затем первый позиционный аргумент — длительность.
                // Аудит 2.11: длинные формы `--kill-after`/`--signal`.
                let mut after_opts = skip_options(rest, &["-k", "-s", "--kill-after", "--signal"]);
                if !after_opts.is_empty() {
                    after_opts.remove(0);
                }
                tokens = after_opts;
            }
            "xargs" => {
                // Аудит 2.11: `-a/--arg-file` и длинные формы опций со
                // значением — иначе значение опции становилось
                // "программой", а настоящая команда уходила в args.
                tokens = skip_options(
                    rest,
                    &[
                        "-I",
                        "-n",
                        "-P",
                        "-s",
                        "-L",
                        "-d",
                        "-a",
                        "--arg-file",
                        "--replace",
                        "--max-args",
                        "--max-procs",
                        "--max-chars",
                        "--max-lines",
                        "--delimiter",
                    ],
                );
            }
            // Аудит 2.13: busybox — мультиплексор, первый аргумент сам
            // является программой (`busybox rm -rf /`, `busybox sh -c`);
            // апплет разбирается на следующей итерации общим путём.
            "busybox" => {
                tokens = rest;
            }
            _ => {
                return Some(Unwrapped::Command {
                    program,
                    args: rest,
                })
            }
        }
    }
    // Аудит 2.10: исчерпание глубины разворачивания — консервативный
    // запрет, не разрешение: обёртки глубже MAX_DEPTH — вырожденная
    // конструкция, единственная практическая цель которой — выйти за
    // пределы анализа. Класс — DeletionOutsideWorkspace: та же строка
    // «недоказуемые цели» (§3.7 п.1).
    Some(Unwrapped::Denied(DenyMatch {
        class: ForbiddenClass::DeletionOutsideWorkspace,
        evidence: format!(
            "глубина обёрток превышает {MAX_DEPTH}: {}",
            tokens.join(" ")
        ),
    }))
}

/// Пропускает опции; опции из `takes_value` проглатывают и следующий токен.
fn skip_options(tokens: Vec<String>, takes_value: &[&str]) -> Vec<String> {
    let mut out = Vec::new();
    let mut iter = tokens.into_iter().peekable();
    while let Some(token) = iter.next() {
        if !token.starts_with('-') {
            out.push(token);
            out.extend(iter);
            break;
        }
        if takes_value.contains(&token.as_str()) {
            iter.next();
        }
    }
    out
}

/// Аудит 2.12: rsync/scp — программы переноса; любой позиционный
/// локальный путь вне области — запись/чтение-перенос вне области.
/// Remote-спецификация `host:path` локальным путём не является (её
/// безопасность — домен сетевого гейта S3, не файловой статики).
fn analyze_transfer(args: &[String], workspace_root: &Path) -> Option<(ForbiddenClass, String)> {
    args.iter()
        .filter(|a| !a.starts_with('-'))
        .filter(|a| !is_remote_spec(a))
        .find(|a| !path_within(a, workspace_root))
        .map(|a| (ForbiddenClass::DeletionOutsideWorkspace, a.clone()))
}

/// `user@host:/path` или `host:/path` — двоеточие до первого слэша.
fn is_remote_spec(arg: &str) -> bool {
    match (arg.find(':'), arg.find('/')) {
        (Some(colon), Some(slash)) => colon < slash,
        (Some(_), None) => true,
        _ => false,
    }
}

/// In-place форма sed: `-i` (включая склейки `-ni`, `-i.bak`) или
/// `--in-place[=...]`.
fn is_in_place_sed(args: &[String]) -> bool {
    args.iter().any(|a| {
        a == "--in-place"
            || a.starts_with("--in-place=")
            || (a.starts_with('-') && !a.starts_with("--") && a[1..].contains('i'))
    })
}

/// sed -i: первый позиционный аргумент — скрипт, далее — файлы целей;
/// файл вне области — модификация вне области.
fn analyze_sed_in_place(
    args: &[String],
    workspace_root: &Path,
) -> Option<(ForbiddenClass, String)> {
    let mut positional = args.iter().filter(|a| !a.starts_with('-'));
    let _script = positional.next();
    positional
        .find(|a| !path_within(a, workspace_root))
        .map(|a| (ForbiddenClass::DeletionOutsideWorkspace, a.clone()))
}

/// tar: запись идёт относительно `-C`/`--directory` (извлечение) и в путь
/// `-f`/`--file` при создании (`c` в флагах или `--create`). Оба канала
/// вне области — модификация вне области.
fn analyze_tar(args: &[String], workspace_root: &Path) -> Option<(ForbiddenClass, String)> {
    let creating = args.iter().any(|a| {
        a == "--create" || (a.starts_with('-') && !a.starts_with("--") && a[1..].contains('c'))
    });
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        let value = match arg.as_str() {
            "-C" | "--directory" => iter.next().map(String::as_str),
            _ if arg.starts_with("--directory=") => Some(&arg["--directory=".len()..]),
            "-f" | "--file" if creating => iter.next().map(String::as_str),
            _ if creating && arg.starts_with("--file=") => Some(&arg["--file=".len()..]),
            _ => None,
        };
        if let Some(path) = value {
            if !path_within(path, workspace_root) {
                return Some((ForbiddenClass::DeletionOutsideWorkspace, path.to_string()));
            }
        }
    }
    None
}

fn is_assignment(token: &str) -> bool {
    let Some((name, _)) = token.split_once('=') else {
        return false;
    };
    !name.is_empty()
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !name.chars().next().unwrap().is_ascii_digit()
}

/// Разбивает текст на отдельные команды цепочки.
fn split_chain(text: &str) -> Vec<String> {
    text.split(['&', '|', ';', '\n'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// Достаёт содержимое `$(...)` и обратных кавычек С ПОДСЧЁТОМ ГЛУБИНЫ —
/// вложенные подоболочки (`$(echo $(sudo …))`) не теряются (находка M2
/// XL-ревью: первый `)` без глубины обрывал разбор ровно на середине).
fn extract_subshells(segment: &str) -> Vec<String> {
    let mut out = Vec::new();
    let chars: Vec<char> = segment.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let is_dollar_paren = chars[i] == '$' && chars.get(i + 1) == Some(&'(');
        let is_backtick = chars[i] == '`';
        if is_dollar_paren || is_backtick {
            let open = if is_dollar_paren { '(' } else { '`' };
            let close = if is_dollar_paren { ')' } else { '`' };
            let start = i + if is_dollar_paren { 2 } else { 1 };
            let mut depth = 1usize;
            let mut j = start;
            while j < chars.len() && depth > 0 {
                if chars[j] == open && is_dollar_paren {
                    depth += 1;
                } else if chars[j] == close {
                    depth -= 1;
                }
                j += 1;
            }
            if depth == 0 {
                out.push(chars[start..j - 1].iter().collect());
            }
            i = j;
        } else {
            i += 1;
        }
    }
    out
}

/// Shell-подобный токенизатор (находка M3 XL-ревью: `r\m`, `'r''m'`,
/// `dd o\f=…` обходили разбор). Кавычки снимаются, склейка смежных строк
/// собирается в один токен, backslash экранирует следующий символ. Это не
/// полный парсер shell — ровно та нормализация, что нужна, чтобы увидеть
/// программу и аргументы так, как их увидит shell.
fn tokenize(segment: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_token = false;
    let mut chars = segment.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                if let Some(next) = chars.next() {
                    current.push(next);
                    in_token = true;
                }
            }
            '\'' => {
                in_token = true;
                for inner in chars.by_ref() {
                    if inner == '\'' {
                        break;
                    }
                    current.push(inner);
                }
            }
            '"' => {
                in_token = true;
                while let Some(inner) = chars.next() {
                    match inner {
                        '"' => break,
                        '\\' => {
                            if let Some(escaped) = chars.next() {
                                current.push(escaped);
                            }
                        }
                        other => current.push(other),
                    }
                }
            }
            c if c.is_whitespace() => {
                if in_token {
                    tokens.push(std::mem::take(&mut current));
                    in_token = false;
                }
            }
            other => {
                current.push(other);
                in_token = true;
            }
        }
    }
    if in_token {
        tokens.push(current);
    }
    tokens
}

fn basename(program: &str) -> String {
    program.rsplit('/').next().unwrap_or(program).to_string()
}

/// Перенаправления вывода: цель проверяется в трёх ипостасях — блочное
/// устройство (запись на устройство), недоказуемая/внешняя цель
/// (модификация вне области, находки M1/M6 XL-ревью), безобидное
/// символьное устройство (пропускается).
fn detect_write_redirects(tokens: &[String], workspace_root: &Path) -> Option<DenyMatch> {
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
            if let Some(m) = classify_write_target(target, workspace_root) {
                return Some(m);
            }
        }
    }
    None
}

/// Общая классификация цели записи: устройство → BlockDeviceWrite;
/// недоказуемо внутри области (`$VAR`, `~`, абсолютный путь вне,
/// `../` наружу) → DeletionOutsideWorkspace; доказуемо безобидная → None.
fn classify_write_target(target: &str, workspace_root: &Path) -> Option<DenyMatch> {
    if is_block_device(target) {
        return Some(DenyMatch {
            class: ForbiddenClass::BlockDeviceWrite,
            evidence: target.to_string(),
        });
    }
    if !path_within(target, workspace_root) && !is_harmless_device(target) {
        return Some(DenyMatch {
            class: ForbiddenClass::DeletionOutsideWorkspace,
            evidence: target.to_string(),
        });
    }
    None
}

/// Блочное устройство по шаблону имени — после ЛЕКСИЧЕСКОЙ нормализации
/// пути (находка C2 XL-ревью: `/dev/./sda`, `/dev//sda` обходили префикс).
/// Дополнительно `/dev/mapper/*` (LVM) и `/dev/disk/*` (by-id/by-path —
/// стабильные пути к тем же устройствам). `/dev/null` и подобные
/// сознательно НЕ блокируются — они не хранят данные ФС.
fn is_block_device(path: &str) -> bool {
    let normalized = normalize_unix_style(path);
    let Some(name) = normalized.strip_prefix("/dev/") else {
        return false;
    };
    // Техдолг TD2.7: `md` (mdadm software RAID, `/dev/md0`) и `nbd`
    // (network block device, `/dev/nbd0`) — блочные устройства того же
    // класса, что уже покрытые префиксы, отсутствовали в списке.
    // `zvol/` (ZFS volumes) — та же природа, что `mapper/`/`disk/`
    // (стабильный путь к блочному устройству через подкаталог, не
    // префикс имени). `mem`/`kmem` (`/dev/mem`, `/dev/kmem`) — НЕ
    // блочное устройство по классу (character device), но прямой доступ
    // к физической памяти ядра — тот же уровень опасности, что запись на
    // блочное устройство, потому проверяется здесь же, а не среди
    // `HARMLESS_CHAR_DEVICES`.
    name.starts_with("mapper/")
        || name.starts_with("disk/")
        || name.starts_with("zvol/")
        || name == "mem"
        || name == "kmem"
        || [
            "sd", "hd", "vd", "xvd", "nvme", "mmcblk", "loop", "dm-", "md", "nbd",
        ]
        .iter()
        .any(|prefix| name.starts_with(prefix))
}

fn is_harmless_device(path: &str) -> bool {
    HARMLESS_CHAR_DEVICES.contains(&normalize_unix_style(path).as_str())
}

/// Строковая (не через `std::path`) нормализация путей POSIX-синтаксиса
/// (`/dev/./sda`, `/dev//sda`) — цели здесь принадлежат тексту shell-команды
/// целевого агента, не хосту, на котором собирается этот бинарник: на
/// Windows `Component::RootDir::as_os_str()` отдаёт `\`, а не `/`, и
/// path-based нормализация ломает распознавание `/dev/*`, даже когда сам
/// анализ выполняется исключительно над текстом, без обращения к ФС.
fn normalize_unix_style(path: &str) -> String {
    let absolute = path.starts_with('/');
    let mut segments: Vec<&str> = Vec::new();
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                segments.pop();
            }
            other => segments.push(other),
        }
    }
    if absolute {
        format!("/{}", segments.join("/"))
    } else {
        segments.join("/")
    }
}

/// Программы, разрушающие ФС. `fdisk`/`parted`/`gdisk`/`sfdisk` работают
/// только с устройствами — безусловны; `mkfs*`/`mke2fs`/`mkswap`/
/// `blkdiscard`/`wipefs` — только когда целью является устройство
/// (`mkswap ./swapfile` внутри области — законная операция).
fn is_filesystem_tool(program: &str, args: &[String]) -> bool {
    if matches!(program, "fdisk" | "parted" | "gdisk" | "sfdisk") {
        return true;
    }
    if program.starts_with("mkfs")
        || matches!(program, "mke2fs" | "mkswap" | "blkdiscard" | "wipefs")
    {
        return args.iter().any(|a| is_block_device(a));
    }
    false
}

/// `dd`: запрещена запись на устройство. Цель с `$`/`~` недоказуемо НЕ
/// устройство — консервативный запрет (находка M1 XL-ревью: правило
/// «недоказуемо = deny» раньше применялось только к `rm`).
fn analyze_dd(args: &[String]) -> Option<(ForbiddenClass, String)> {
    args.iter().find_map(|a| {
        a.strip_prefix("of=").and_then(|target| {
            if is_block_device(target) || target.contains('$') || target.contains('~') {
                Some((ForbiddenClass::BlockDeviceWrite, target.to_string()))
            } else {
                None
            }
        })
    })
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

/// `find` с `-delete` или `-exec rm` (находка C3 XL-ревью): корни поиска —
/// позиционные аргументы до первой опции — классифицируются как цели `rm`.
fn analyze_find(args: &[String], workspace_root: &Path) -> Option<DenyMatch> {
    // Техдолг TD2.5: `-execdir`/`-ok`/`-okdir` действуют так же, как
    // `-exec` (исполняют команду на найденном; `-ok`/`-okdir` — с
    // интерактивным подтверждением, что не отменяет опасность самой
    // возможности — подтверждение здесь не то же самое, что подтверждение
    // capability-слоя) — раньше проверялось точное совпадение со строкой
    // `"-exec"`, эти три формы проходили незамеченными.
    let deletes = args.iter().any(|a| a == "-delete")
        || args.windows(2).any(|w| {
            matches!(w[0].as_str(), "-exec" | "-execdir" | "-ok" | "-okdir")
                && basename(&w[1]) == "rm"
        });
    if !deletes {
        return None;
    }
    for root in args.iter().take_while(|a| !a.starts_with('-')) {
        if root.as_str() == "/" || root.as_str() == "/*" {
            return Some(DenyMatch {
                class: ForbiddenClass::FilesystemDestruction,
                evidence: root.clone(),
            });
        }
        if !path_within(root, workspace_root) {
            return Some(DenyMatch {
                class: ForbiddenClass::DeletionOutsideWorkspace,
                evidence: root.clone(),
            });
        }
    }
    None
}

/// `tee` — все позиционные аргументы суть цели записи.
fn analyze_write_targets(
    args: &[String],
    workspace_root: &Path,
) -> Option<(ForbiddenClass, String)> {
    args.iter()
        .filter(|a| !a.starts_with('-'))
        .find_map(|a| classify_write_target(a, workspace_root).map(|m| (m.class, m.evidence)))
}

/// `cp`/`mv`/`install` — запрет направлен на ПРИЁМНИК (последний
/// позиционный аргумент или значение `-t`): чтение извне области законно,
/// запись вовне — нет (находка M6 XL-ревью).
fn analyze_copy(args: &[String], workspace_root: &Path) -> Option<(ForbiddenClass, String)> {
    // Техдолг TD2.6: только короткая форма `-t DEST` (отдельным токеном)
    // распознавалась как явный приёмник — длинная форма `--target-directory=DEST`
    // (значение внутри самого токена, начинается с `-`, поэтому не
    // попадала и в резервный `rfind` "последний не-`-`-аргумент") и
    // `--target-directory DEST` (отдельным токеном) проходили
    // незамеченными.
    let dest = if let Some(pos) = args
        .iter()
        .position(|a| a == "-t" || a == "--target-directory")
    {
        args.get(pos + 1).cloned()
    } else if let Some(value) = args
        .iter()
        .find_map(|a| a.strip_prefix("--target-directory="))
    {
        Some(value.to_string())
    } else {
        args.iter().rfind(|a| !a.starts_with('-')).cloned()
    };
    dest.and_then(|d| classify_write_target(&d, workspace_root).map(|m| (m.class, m.evidence)))
}

/// Техдолг TD2.3: `setpriv --reuid 0`/`--regid 0`/`--clear-groups` меняют
/// эффективный uid/gid процесса, `unshare -r`/`--map-root-user` мапят
/// root ВНУТРИ нового user-namespace — оба пути дают процессу права,
/// которых у вызывающего кода не было. `unshare -n`/`-m` (сетевой/
/// mount namespace без смены пользователя) сознательно НЕ считаются
/// эскалацией привилегий — другой класс изоляции.
fn changes_privilege_scope(args: &[String]) -> bool {
    args.iter().any(|a| {
        matches!(
            a.as_str(),
            "-r" | "--map-root-user" | "--reuid" | "--regid" | "--clear-groups"
        ) || a.starts_with("--reuid=")
            || a.starts_with("--regid=")
    })
}

/// Владелец root: по имени, по числовому uid, по группе (находка M4
/// XL-ревью: `chown 0:0`, `chown :root` проходили).
fn is_root_owner(spec: &str) -> bool {
    spec == "root"
        || spec.starts_with("root:")
        || spec.starts_with("root.") // legacy-сепаратор (2.17 аудита)
        || spec == "0"
        || spec.starts_with("0:")
        || spec.starts_with("0.")
        || spec.starts_with(":root")
        || spec.starts_with(":0")
}

/// setuid/setgid: символический режим (`u+s`, `g+s`, `a+s`, `+s`) или
/// числовой со старшим битом — ведущие нули не мешают (`chmod 04755`,
/// находка M4 XL-ревью).
///
/// Техдолг TD2.4: символьная ветка раньше требовала, чтобы `s` был
/// ПОСЛЕДНИМ символом сразу после `+` (буквально `part.ends_with("+s")`)
/// — `u+sx`/`a+xs` (setuid вместе с другим битом в той же группе) и
/// `u=s`/`=s` (оператор `=`, не `+`) не ловились. Теперь находится
/// оператор (`+` или `=` — `-` означает СНЯТИЕ бита, не установку, не
/// опасно) и проверяется, что `s` есть СРЕДИ добавляемых битов, а не
/// обязательно последним.
fn is_setuid_chmod(args: &[String]) -> bool {
    args.iter().any(|a| {
        let a = a.trim_start_matches('-');
        let symbolic = a.split(',').any(|part| {
            let Some(op_pos) = part.find(['+', '=']) else {
                return false;
            };
            let (who, rest) = part.split_at(op_pos);
            let perms = &rest[1..];
            who.chars().all(|c| "ugoa".contains(c)) && perms.contains('s')
        });
        let digits: String = a.trim_start_matches('0').to_string();
        let numeric = !digits.is_empty()
            && digits.chars().all(|c| c.is_ascii_digit())
            && digits.len() >= 4
            && matches!(digits.chars().next(), Some('2' | '4' | '6'));
        symbolic || numeric
    })
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

/// Лексическая нормализация: `.` убираются, `..` схлопываются, двойные
/// слэши сливаются — без обращения к ФС.
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

/// Fork-бомба: объявление функции `name(){`, чьё тело фоново (`&`)
/// вызывает само себя — с конвейером (`:(){ :|:& };:`) или без
/// (`f(){ f&f;f };f`, находка m7 XL-ревью). Самовызов определяется по
/// повторному вхождению имени в тело.
fn detect_fork_bomb(text: &str) -> Option<DenyMatch> {
    let compact: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    let mut search_from = 0;
    while let Some(pos) = compact[search_from..].find("(){") {
        let def_start = search_from + pos;
        let name: String = compact[..def_start]
            .chars()
            .rev()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == ':')
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        let body_start = def_start + 3;
        let body_end = compact[body_start..]
            .find('}')
            .map(|e| body_start + e)
            .unwrap_or(compact.len());
        let body = &compact[body_start..body_end];
        if body.contains('&') && !name.is_empty() && body.contains(&name) {
            return Some(DenyMatch {
                class: ForbiddenClass::ForkBomb,
                evidence: text.trim().to_string(),
            });
        }
        search_from = body_start;
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

    // XL-ревью 2026-08-13 HIGH #2: контрольные файлы гейта.
    #[test]
    fn control_file_write_via_files_tool_is_denied() {
        for target in [".berimor/allow", ".berimor-allow", "./.berimor/allow"] {
            let action = ProposedAction {
                tool: "files.write".into(),
                args: json!({"path": target, "content": "*\n"}),
                mutates: true,
            };
            let m = analyze(&action, Path::new("/workspace")).unwrap();
            assert_eq!(m.class, ForbiddenClass::PrivilegeEscalation, "{target}");
        }
    }

    #[test]
    fn control_file_write_via_shell_is_denied_read_is_free() {
        let write = ProposedAction {
            tool: "terminal.exec".into(),
            args: json!({"command": "echo '*' > .berimor/allow"}),
            mutates: true,
        };
        let m = analyze(&write, Path::new("/workspace")).unwrap();
        assert_eq!(m.class, ForbiddenClass::PrivilegeEscalation);
        // Чтение контрольного файла — легитимно (секретов там нет).
        let read = ProposedAction {
            tool: "terminal.exec".into(),
            args: json!({"command": "cat .berimor/allow"}),
            mutates: true,
        };
        assert!(analyze(&read, Path::new("/workspace")).is_none());
        // Обычный файл рядом — не цель правила.
        let normal = ProposedAction {
            tool: "files.write".into(),
            args: json!({"path": "src/allow.rs", "content": "x"}),
            mutates: true,
        };
        assert!(analyze(&normal, Path::new("/workspace")).is_none());
    }

    #[test]
    fn path_escape_denied_even_when_caller_forgets_mutates_flag() {
        // Находка m10 XL-ревью: имя инструмента подстраховывает флаг.
        let action = ProposedAction {
            tool: "files.delete".into(),
            args: json!({"path": "/etc/passwd"}),
            mutates: false,
        };
        let m = analyze(&action, Path::new("/workspace")).unwrap();
        assert_eq!(m.class, ForbiddenClass::DeletionOutsideWorkspace);
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

    #[test]
    fn tokenizer_unescapes_and_concatenates_quotes() {
        assert_eq!(tokenize("r\\m -rf /"), vec!["rm", "-rf", "/"]);
        assert_eq!(tokenize("'r''m' -rf /"), vec!["rm", "-rf", "/"]);
        assert_eq!(tokenize("dd o\\f=/dev/sda"), vec!["dd", "of=/dev/sda"]);
        assert_eq!(
            tokenize("bash -c \"rm -rf /\""),
            vec!["bash", "-c", "rm -rf /"]
        );
    }
}
