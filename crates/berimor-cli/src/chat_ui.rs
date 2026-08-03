//! Консольный интерфейс чата (§20.13) — уровень современных agentic CLI:
//! баннер, цветовая тема, спиннер раздумий, живой вывод вызовов
//! инструментов, рендер markdown-lite ответов. Без зависимостей: ANSI
//! напрямую, отключение — по NO_COLOR / не-терминалу / TERM=dumb (как
//! у crossterm/anstream, но без крейта ради 200 строк).

use std::io::IsTerminal;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Цветовая тема. Пустые строки = стили выключены (не-терминал,
/// NO_COLOR, TERM=dumb): вывод остаётся чистым текстом для пайпов,
/// журналов и e2e-тестов.
#[derive(Debug, Clone, Default)]
pub struct Theme {
    pub enabled: bool,
    pub reset: &'static str,
    pub bold: &'static str,
    pub dim: &'static str,
    pub cyan: &'static str,
    pub green: &'static str,
    pub yellow: &'static str,
    pub magenta: &'static str,
    pub gray_bg: &'static str,
}

impl Theme {
    pub fn detect() -> Self {
        let colored = std::io::stderr().is_terminal()
            && std::env::var_os("NO_COLOR").is_none()
            && std::env::var("TERM").map(|t| t != "dumb").unwrap_or(true);
        if colored {
            Self {
                enabled: true,
                reset: "\x1b[0m",
                bold: "\x1b[1m",
                dim: "\x1b[2m",
                cyan: "\x1b[36m",
                green: "\x1b[32m",
                yellow: "\x1b[33m",
                magenta: "\x1b[35m",
                gray_bg: "\x1b[48;5;236m",
            }
        } else {
            Self::default()
        }
    }

    pub fn paint(&self, style: &str, text: &str) -> String {
        if self.enabled {
            format!("{style}{text}{}", self.reset)
        } else {
            text.to_string()
        }
    }

    pub fn bold(&self, text: &str) -> String {
        self.paint(self.bold, text)
    }
    pub fn dim(&self, text: &str) -> String {
        self.paint(self.dim, text)
    }
    pub fn cyan(&self, text: &str) -> String {
        self.paint(self.cyan, text)
    }
    pub fn green(&self, text: &str) -> String {
        self.paint(self.green, text)
    }
    pub fn yellow(&self, text: &str) -> String {
        self.paint(self.yellow, text)
    }
    pub fn magenta(&self, text: &str) -> String {
        self.paint(self.magenta, text)
    }
}

const LOGO: &str = r"
██████╗ ███████╗██████╗ ██╗███╗   ███╗ ██████╗ ██████╗
██╔══██╗██╔════╝██╔══██╗██║████╗ ████║██╔═══██╗██╔══██╗
██████╔╝█████╗  ██████╔╝██║██╔████╔██║██║   ██║██████╔╝
██╔══██╗██╔══╝  ██╔══██╗██║██║╚██╔╝██║██║   ██║██╔══██╗
██████╔╝███████╗██║  ██║██║██║ ╚═╝ ██║╚██████╔╝██║  ██║
╚═════╝ ╚══════╝╚═╝  ╚═╝╚═╝╚═╝     ╚═╝ ╚═════╝ ╚═╝  ╚═╝";

/// Стартовый баннер сессии: логотип, версия, область, режим.
pub fn print_banner(theme: &Theme, workspace: &str, tools: &str, journal: &str) {
    eprintln!("{}", theme.cyan(LOGO));
    eprintln!(
        "{}",
        theme.bold(&format!("  berimor v{}", env!("CARGO_PKG_VERSION")))
    );
    eprintln!("  {}", theme.dim("детерминированное ядро · аудит · replay"));
    eprintln!();
    eprintln!("  {} {}", theme.magenta("область:"), workspace);
    eprintln!("  {} {}", theme.magenta("инструменты:"), tools);
    eprintln!("  {} {}", theme.magenta("журнал:"), journal);
    eprintln!();
    eprintln!(
        "  {}",
        theme.dim("/help — команды · /exit или Ctrl+D — выход")
    );
    eprintln!();
}

/// Одна строка активности инструмента (в реальном времени, между
/// сообщением пользователя и ответом агента): dim, компактно.
pub fn print_tool_turn(theme: &Theme, tool: &str, args_summary: &str, ok: bool) {
    let mark = if ok {
        theme.green("✓")
    } else {
        theme.yellow("✗")
    };
    eprintln!(
        "  {} {}",
        mark,
        theme.dim(&format!("{tool}({args_summary})"))
    );
}

/// Краткая форма аргументов для живого вывода: одна строка, потолок
/// 80 символов (полные аргументы — в журнале, не на экране).
pub fn summarize_args(args: &serde_json::Value) -> String {
    let text = match args {
        serde_json::Value::Object(map) if map.is_empty() => String::new(),
        other => other.to_string(),
    };
    let single_line = text.replace('\n', " ");
    if single_line.chars().count() > 80 {
        format!("{}…", single_line.chars().take(79).collect::<String>())
    } else {
        single_line
    }
}

/// Спиннер раздумий: кадры braille в stderr, пока агент работает.
/// Drop останавливает поток и стирает строку — в журнал (не-терминал)
/// спиннер не пишется вовсе (Theme::detect).
pub struct Spinner {
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Spinner {
    pub fn start(theme: &Theme, label: &str) -> Self {
        if !theme.enabled {
            return Self {
                stop: Arc::new(AtomicBool::new(true)),
                handle: None,
            };
        }
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let label = label.to_string();
        let handle = std::thread::spawn(move || {
            const FRAMES: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
            let mut frame = 0;
            while !flag.load(Ordering::Relaxed) {
                eprint!(
                    "\r  \x1b[36m{}\x1b[0m \x1b[2m{label}\x1b[0m",
                    FRAMES[frame % FRAMES.len()]
                );
                let _ = std::io::Write::flush(&mut std::io::stderr());
                frame += 1;
                std::thread::sleep(Duration::from_millis(80));
            }
            eprint!("\r\x1b[2K");
            let _ = std::io::Write::flush(&mut std::io::stderr());
        });
        Self {
            stop,
            handle: Some(handle),
        }
    }
}

impl Drop for Spinner {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// Markdown-lite для ответов агента: fenced-блоки кода (фон), заголовки
/// (cyan+bold), **полужирный**, `инлайн-код`, списки остаются как есть.
/// Сознательно неполный markdown: полноценный рендерер — отдельная
/// задача, здесь цель — читаемость типового ответа агента.
pub fn render_markdown(theme: &Theme, text: &str) -> String {
    let mut out = String::new();
    let mut in_code = false;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            in_code = !in_code;
            continue; // ограждение само по себе не печатаем
        }
        if in_code {
            if theme.enabled {
                out.push_str(&format!("  {} {}{}\n", theme.gray_bg, line, theme.reset));
            } else {
                out.push_str(&format!("  {line}\n"));
            }
            continue;
        }
        if let Some(header) = line.strip_prefix("## ") {
            out.push_str(&format!("{}\n", theme.cyan(&theme.bold(header))));
        } else if let Some(header) = line.strip_prefix("# ") {
            out.push_str(&format!("{}\n", theme.cyan(&theme.bold(header))));
        } else {
            out.push_str(&render_inline(theme, line));
            out.push('\n');
        }
    }
    out.trim_end_matches('\n').to_string()
}

fn render_inline(theme: &Theme, line: &str) -> String {
    if !theme.enabled {
        return line.to_string();
    }
    let mut out = String::new();
    let mut rest = line;
    loop {
        // Ближайший из маркеров ** и ` — простой однопроходный разбор.
        let bold_pos = rest.find("**");
        let code_pos = rest.find('`');
        let (pos, is_bold) = match (bold_pos, code_pos) {
            (Some(b), Some(c)) if b <= c => (b, true),
            (Some(_), Some(c)) => (c, false),
            (Some(b), None) => (b, true),
            (None, Some(c)) => (c, false),
            (None, None) => {
                out.push_str(rest);
                return out;
            }
        };
        out.push_str(&rest[..pos]);
        let marker_len = if is_bold { 2 } else { 1 };
        let after = &rest[pos + marker_len..];
        let close = if is_bold {
            after.find("**")
        } else {
            after.find('`')
        };
        match close {
            Some(end) => {
                let segment = &after[..end];
                if is_bold {
                    out.push_str(&theme.bold(segment));
                } else {
                    out.push_str(&theme.yellow(segment));
                }
                rest = &after[end + marker_len..];
            }
            None => {
                // Незакрытый маркер — как обычный текст.
                out.push_str(&rest[pos..]);
                return out;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn colored() -> Theme {
        Theme {
            enabled: true,
            reset: "\x1b[0m",
            bold: "\x1b[1m",
            dim: "\x1b[2m",
            cyan: "\x1b[36m",
            green: "\x1b[32m",
            yellow: "\x1b[33m",
            magenta: "\x1b[35m",
            gray_bg: "\x1b[48;5;236m",
        }
    }

    #[test]
    fn disabled_theme_is_pure_passthrough() {
        let theme = Theme::default();
        let text = "## Заголовок\n**жирный** и `код`\n```\nblock\n```";
        let rendered = render_markdown(&theme, text);
        assert!(
            !rendered.contains("\x1b"),
            "без темы — без ANSI: {rendered:?}"
        );
        assert!(rendered.contains("жирный"));
        assert!(rendered.contains("block"));
    }

    #[test]
    fn code_fences_are_not_printed_but_content_is() {
        let theme = colored();
        let rendered = render_markdown(&theme, "до\n```rust\nlet x = 1;\n```\nпосле");
        assert!(!rendered.contains("```"));
        assert!(rendered.contains("let x = 1;"));
        assert!(rendered.contains("до"));
        assert!(rendered.contains("после"));
    }

    #[test]
    fn inline_bold_and_code_get_styles() {
        let theme = colored();
        let rendered = render_inline(&theme, "сделал **важное** через `files.write`");
        assert!(rendered.contains("\x1b[1mважное\x1b[0m"));
        assert!(rendered.contains("\x1b[33mfiles.write\x1b[0m"));
    }

    #[test]
    fn unclosed_marker_stays_literal() {
        let theme = colored();
        let rendered = render_inline(&theme, "звёздочки ** не маркер");
        assert!(rendered.contains("** не маркер"));
    }

    #[test]
    fn summarize_args_truncates_and_flattens() {
        let long = serde_json::json!({"command": "x".repeat(200)});
        let summary = summarize_args(&long);
        assert!(summary.chars().count() <= 81);
        assert!(!summary.contains('\n'));
        assert_eq!(summarize_args(&serde_json::json!({})), "");
    }
}
