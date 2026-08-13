//! Инструмент `human.ask` (спека `docs/rnd/builtin-tools-waves-spec.md`, B7):
//! запрос свободного ответа пользователя — отдельный канал, НЕ
//! capability-гейт (тот решает допуск, тут — данные от человека).
//!
//! Модуль даёт:
//! - [`HumanAsker`] — точка подключения канала ввода (REPL, TUI, тесты);
//! - [`HumanAskDispatch`] — диспетчер-обёртка (прецедент
//!   [`crate::agent_dispatch::AgentRunDispatch`]): `human.ask` исполняет
//!   сам через asker, остальные инструменты пробрасывает в inner;
//! - [`StdinAsker`] — REPL-реализация (stderr-вопрос + stdin-ответ);
//!   TUI-реализация — клей родителя (WorkerMsg::AskRequest + модал).
//!
//! Args: `{question: string, options?: [string]}` — options добавляются
//! в текст вопроса перечнем; ответ: `{answer}`. mutates: **false**
//! (инструмент только читает ввод человека, ничего не изменяет).

// Проводка (mod + цепочка диспетчеров) — клей родителя; до неё публичные
// типы модуля не задействованы в бинаре, только в тестах.
#![allow(dead_code)]

use berimor_executors::tool_only::{DispatchError, ToolDispatch};
use serde_json::{json, Value};
use std::io::{BufRead, Write};

/// Канал запроса свободного ответа пользователя. Отдельно от
/// capability-гейта: гейт решает допуск действия, здесь — данные от
/// человека.
pub trait HumanAsker: Send + Sync {
    /// Задать вопрос и дождаться ответа; ошибка — текст причины
    /// (например, EOF на вводе или отказ канала).
    fn ask(&self, question: &str) -> Result<String, String>;
}

/// Диспетчер-обёртка: `human.ask` исполняет сам через [`HumanAsker`],
/// остальные инструменты делегирует внутренней цепочке.
pub struct HumanAskDispatch<'a> {
    pub asker: &'a dyn HumanAsker,
    pub inner: &'a dyn ToolDispatch,
}

impl HumanAskDispatch<'_> {
    fn ask_human(&self, args: &Value) -> Result<Value, DispatchError> {
        let question = args
            .get("question")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                crate::builtin_dispatch::err_str("human.ask", "аргумент question обязателен")
            })?;

        // options — необязательный перечень вариантов; добавляется в
        // текст вопроса списком, чтобы человек видел выбор.
        let mut text = question.to_string();
        if let Some(options) = args.get("options") {
            let options = options.as_array().ok_or_else(|| {
                crate::builtin_dispatch::err_str(
                    "human.ask",
                    "аргумент options должен быть массивом строк",
                )
            })?;
            if !options.is_empty() {
                text.push_str("\nВарианты:");
                for (i, option) in options.iter().enumerate() {
                    let option = option.as_str().ok_or_else(|| {
                        crate::builtin_dispatch::err_str(
                            "human.ask",
                            format!("options[{i}] должен быть строкой"),
                        )
                    })?;
                    text.push_str(&format!("\n{}. {}", i + 1, option));
                }
            }
        }

        let answer = self
            .asker
            .ask(&text)
            .map_err(|e| crate::builtin_dispatch::err_str("human.ask", e))?;
        Ok(json!({"answer": answer}))
    }
}

impl ToolDispatch for HumanAskDispatch<'_> {
    fn call(&self, tool: &str, args: &Value) -> Result<Value, DispatchError> {
        if tool == "human.ask" {
            return self.ask_human(args);
        }
        self.inner.call(tool, args)
    }
}

/// REPL-реализация канала: вопрос в stderr (не путается с выводом
/// программы), ответ — строка из stdin. EOF — ошибка.
pub struct StdinAsker;

impl HumanAsker for StdinAsker {
    fn ask(&self, question: &str) -> Result<String, String> {
        eprintln!("{question}");
        eprint!("> ");
        let _ = std::io::stderr().flush();
        let mut line = String::new();
        let read = std::io::stdin()
            .lock()
            .read_line(&mut line)
            .map_err(|e| format!("чтение ответа из stdin: {e}"))?;
        if read == 0 {
            return Err("ввод завершён (EOF) — ответ не получен".into());
        }
        Ok(line.trim_end_matches(['\n', '\r']).to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use berimor_executors::tool_only::StaticToolDispatch;
    use std::sync::Mutex;

    /// Фейковый канал: возвращает заданный ответ и записывает полученный
    /// вопрос (проверка склейки options в текст).
    struct FakeAsker {
        answer: String,
        asked: Mutex<Vec<String>>,
    }

    impl FakeAsker {
        fn new(answer: &str) -> Self {
            Self {
                answer: answer.to_string(),
                asked: Mutex::new(Vec::new()),
            }
        }

        fn asked(&self) -> Vec<String> {
            self.asked.lock().expect("мьютекс").clone()
        }
    }

    impl HumanAsker for FakeAsker {
        fn ask(&self, question: &str) -> Result<String, String> {
            self.asked
                .lock()
                .expect("мьютекс")
                .push(question.to_string());
            Ok(self.answer.clone())
        }
    }

    /// Фейковый канал, всегда завершающийся ошибкой.
    struct FailingAsker;

    impl HumanAsker for FailingAsker {
        fn ask(&self, _question: &str) -> Result<String, String> {
            Err("канал ввода недоступен".into())
        }
    }

    fn inner_stub() -> StaticToolDispatch {
        StaticToolDispatch::new(vec![("other.tool".to_string(), json!({"ok": true}), false)])
    }

    #[test]
    fn human_ask_возвращает_ответ_asker() {
        let asker = FakeAsker::new("да, продолжай");
        let inner = inner_stub();
        let dispatch = HumanAskDispatch {
            asker: &asker,
            inner: &inner,
        };
        let out = dispatch
            .call("human.ask", &json!({"question": "Продолжить?"}))
            .expect("ответ");
        assert_eq!(out, json!({"answer": "да, продолжай"}));
        assert_eq!(asker.asked(), vec!["Продолжить?".to_string()]);
    }

    #[test]
    fn human_ask_добавляет_options_в_текст_вопроса() {
        let asker = FakeAsker::new("2");
        let inner = inner_stub();
        let dispatch = HumanAskDispatch {
            asker: &asker,
            inner: &inner,
        };
        let out = dispatch
            .call(
                "human.ask",
                &json!({"question": "Какой вариант?", "options": ["первый", "второй"]}),
            )
            .expect("ответ");
        assert_eq!(out, json!({"answer": "2"}));
        let asked = asker.asked();
        assert_eq!(asked.len(), 1);
        assert_eq!(asked[0], "Какой вариант?\nВарианты:\n1. первый\n2. второй");
    }

    #[test]
    fn human_ask_без_question_ошибка() {
        let asker = FakeAsker::new("ответ");
        let inner = inner_stub();
        let dispatch = HumanAskDispatch {
            asker: &asker,
            inner: &inner,
        };
        let err = dispatch
            .call("human.ask", &json!({}))
            .expect_err("должна быть ошибка");
        assert_eq!(err.tool, "human.ask");
        assert!(err.reason.contains("question"), "текст: {}", err.reason);
    }

    #[test]
    fn human_ask_ошибка_asker_становится_dispatch_error() {
        let asker = FailingAsker;
        let inner = inner_stub();
        let dispatch = HumanAskDispatch {
            asker: &asker,
            inner: &inner,
        };
        let err = dispatch
            .call("human.ask", &json!({"question": "Вопрос?"}))
            .expect_err("должна быть ошибка");
        assert_eq!(err.tool, "human.ask");
        assert!(err.reason.contains("недоступен"), "текст: {}", err.reason);
    }

    #[test]
    fn не_human_ask_пробрасывается_в_inner() {
        let asker = FakeAsker::new("не должно вызываться");
        let inner = inner_stub();
        let dispatch = HumanAskDispatch {
            asker: &asker,
            inner: &inner,
        };
        let out = dispatch
            .call("other.tool", &json!({"x": 1}))
            .expect("ответ заглушки");
        assert_eq!(out, json!({"ok": true}));
        assert!(asker.asked().is_empty(), "asker не должен вызываться");
    }

    #[test]
    fn options_не_массив_ошибка() {
        let asker = FakeAsker::new("ответ");
        let inner = inner_stub();
        let dispatch = HumanAskDispatch {
            asker: &asker,
            inner: &inner,
        };
        let err = dispatch
            .call(
                "human.ask",
                &json!({"question": "Вопрос?", "options": "не массив"}),
            )
            .expect_err("должна быть ошибка");
        assert_eq!(err.tool, "human.ask");
        assert!(err.reason.contains("массив"), "текст: {}", err.reason);
    }
}
