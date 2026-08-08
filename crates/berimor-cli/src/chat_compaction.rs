//! Компакция ленты чата (prompt-next-wave.md задача 4): при
//! переполнении бюджета контекста старая часть ленты сжимается вызовом
//! модели (`HistorySummary`) в одну запись, вместо молчаливого
//! посимвольного усечения `context_engine::apply_budget` (то, которое
//! уже происходит с `task_state` целиком сегодня — режет С КОНЦА
//! сериализованного JSON без учёта смысла, могло бы обрезать `goal`).
//!
//! Логика (эта крейта) не знает, ЧЕМ вызывается модель — `summarize`
//! внедряется вызывающим кодом (тот же приём, что `serve::bind`/
//! `memory::consolidate_with_embedder`): тестируется мок-функцией, без
//! реального `ModelPool`/провайдера.

use serde_json::{json, Value};

/// Маркер сжатой предыстории — виден и в самой ленте (модель видит его
/// на следующих ходах), и человеку при просмотре истории.
pub const COMPACTED_MARKER: &str = "[предыстория сжата]";

/// Триггер компакции — суммарный размер сериализованной ленты в
/// символах. Стартовая константа кода (тот же класс, что
/// `context_engine::budget_chars`, `DEFAULT_SIMILARITY_THRESHOLD`) —
/// половина бюджета Strong-класса (32 000): лента делит бюджет слоя
/// `task_state` с `goal`/`tools`, не единственный потребитель.
pub const COMPACTION_TRIGGER_CHARS: usize = 16_000;

/// Сколько последних записей (не ходов — одна запись на реплику)
/// оставить нетронутыми при компакции. Достаточно для непосредственной
/// связности следующего ответа модели.
pub const KEEP_RECENT_ENTRIES: usize = 6;

/// Сжимает `history`, если она превышает [`COMPACTION_TRIGGER_CHARS`]:
/// все записи, кроме последних [`KEEP_RECENT_ENTRIES`], заменяются
/// ОДНОЙ записью — маркер + суммаризация от `summarize`. Возвращает
/// `true`, если компакция реально произошла (и лента изменилась).
///
/// Сбой `summarize` — НЕ ошибка компакции целиком: лента остаётся как
/// есть (тот же принцип, что «сбой памяти не хоронит ход» — сбой
/// суммаризации не хоронит сессию чата), предупреждение — вызывающему
/// коду через возвращаемое `Err`, печатать его или нет — его выбор.
pub fn compact_if_needed(
    history: &mut Vec<Value>,
    summarize: &dyn Fn(&[Value]) -> Result<String, String>,
) -> Result<bool, String> {
    if history.len() <= KEEP_RECENT_ENTRIES {
        return Ok(false);
    }
    let total_chars: usize = history.iter().map(|entry| entry.to_string().len()).sum();
    if total_chars <= COMPACTION_TRIGGER_CHARS {
        return Ok(false);
    }

    let split_at = history.len() - KEEP_RECENT_ENTRIES;
    let summary = summarize(&history[..split_at])?;

    let recent: Vec<Value> = history[split_at..].to_vec();
    history.clear();
    history.push(json!({
        "role": "system",
        "content": format!("{COMPACTED_MARKER} {summary}")
    }));
    history.extend(recent);
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(role: &str, content: &str) -> Value {
        json!({"role": role, "content": content})
    }

    fn fill(history: &mut Vec<Value>, pairs: usize, filler_len: usize) {
        let filler = "x".repeat(filler_len);
        for i in 0..pairs {
            history.push(entry("user", &format!("вопрос {i} {filler}")));
            history.push(entry("assistant", &format!("ответ {i} {filler}")));
        }
    }

    #[test]
    fn short_history_is_not_compacted() {
        let mut history = vec![entry("user", "привет"), entry("assistant", "здравствуйте")];
        let summarize = |_: &[Value]| -> Result<String, String> {
            panic!("summarize не должен вызываться — лента короткая")
        };

        let changed = compact_if_needed(&mut history, &summarize).unwrap();

        assert!(!changed);
        assert_eq!(history.len(), 2);
    }

    #[test]
    fn long_history_over_threshold_is_compacted_keeping_recent_entries_verbatim() {
        let mut history = Vec::new();
        fill(&mut history, 30, 500); // существенно больше порога
        let original_len = history.len();
        let recent_before: Vec<Value> = history[original_len - KEEP_RECENT_ENTRIES..].to_vec();
        let summarize = |old: &[Value]| -> Result<String, String> {
            Ok(format!("сжато {} записей", old.len()))
        };

        let changed = compact_if_needed(&mut history, &summarize).unwrap();

        assert!(changed);
        // 1 запись-маркер + KEEP_RECENT_ENTRIES нетронутых.
        assert_eq!(history.len(), 1 + KEEP_RECENT_ENTRIES);
        assert_eq!(history[0]["role"], "system");
        assert!(history[0]["content"]
            .as_str()
            .unwrap()
            .starts_with(COMPACTED_MARKER));
        assert!(history[0]["content"].as_str().unwrap().contains("сжато"));
        // Последние записи — те же самые, не переписаны и не потеряны.
        assert_eq!(&history[1..], &recent_before[..]);
    }

    #[test]
    fn summarizer_failure_leaves_history_untouched() {
        let mut history = Vec::new();
        fill(&mut history, 30, 500);
        let original = history.clone();
        let summarize = |_: &[Value]| -> Result<String, String> {
            Err("модель недоступна".to_string())
        };

        let result = compact_if_needed(&mut history, &summarize);

        assert!(result.is_err());
        assert_eq!(
            history, original,
            "сбой суммаризации не должен менять ленту"
        );
    }

    #[test]
    fn history_exactly_at_keep_recent_count_is_not_compacted_even_if_large() {
        let mut history = Vec::new();
        // KEEP_RECENT_ENTRIES записей, но каждая огромная — превышает
        // порог по символам, но компактировать нечего (нечего резать,
        // всё и так «недавнее»).
        for i in 0..KEEP_RECENT_ENTRIES {
            history.push(entry("user", &format!("{} {}", i, "x".repeat(5_000))));
        }
        let summarize = |_: &[Value]| -> Result<String, String> {
            panic!("summarize не должен вызываться — резать нечего")
        };

        let changed = compact_if_needed(&mut history, &summarize).unwrap();

        assert!(!changed);
    }
}
