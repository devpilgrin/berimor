//! Рабочая память: состояние процесса/сессии, сворачивание при переполнении бюджета.
//!
//! Источник: `docs/arch/memory-model.md` §4. ROADMAP: MEM1.
//!
//! «Оригинал остаётся в эпизодическом слое — потеря невозможна» (§4):
//! этот модуль не хранит историю сам — история уже журнал (`EventLog`,
//! F1, индексируемый MEM2). Его работа — решить, когда пора сворачивать
//! (бюджет) и как собрать заменяющую запись из уже полученной сводки.
//! Сам вызов модели — `StructuredLLM`/E2 с контрактом
//! `WorkingMemorySummary` (`berimor-mediation::contracts`, MEM1) — задача
//! вызывающего кода (CLI-интеграция), не этого модуля: так же, как
//! `pipeline::mediate` (M6) не вызывает модель повторно сам при `Retry`.

use berimor_types::event::EventSeq;

/// Один элемент истории, ожидающий возможного сворачивания. `text` — уже
/// отформатированный кусок контекста (то, что реально пошло бы в
/// подсказку модели), не сырое событие целиком.
#[derive(Debug, Clone, PartialEq)]
pub struct HistoryEntry {
    pub seq: EventSeq,
    pub text: String,
}

/// Суммарный объём истории — то же измерение, что и бюджет класса
/// модели (`berimor_context_engine::budget_chars`, C3), с которым
/// сравнивается результат. 4.6 аудита: БАЙТЫ (`str::len`), не символы —
/// см. контракт в `berimor_context_engine::total_chars`.
pub fn total_chars(entries: &[HistoryEntry]) -> usize {
    entries.iter().map(|e| e.text.len()).sum()
}

/// Пора ли сворачивать: суммарный объём истории строго превысил бюджет.
/// Точное равенство бюджету ещё не переполнение — документ не уточняет
/// более точной границы, чем «при приближении к бюджету»; строгое `>` —
/// минимальное определённое поведение: сжатие начинается, как только
/// бюджета перестало хватать, не раньше и не с произвольным запасом,
/// который нечем обосновать.
pub fn over_budget(entries: &[HistoryEntry], budget_chars: usize) -> bool {
    total_chars(entries) > budget_chars
}

/// Результат сворачивания: сводка заменяет исходные записи истории в
/// РАБОЧЕЙ памяти (не в эпизодической — там они остаются навсегда;
/// свёртка лишь перестаёт тащить их полный текст дальше в подсказку).
#[derive(Debug, Clone, PartialEq)]
pub struct CollapsedEntry {
    pub summary: String,
    /// Самая поздняя из свёрнутых записей — граница, до которой
    /// (включительно) история заменена сводкой; всё после неё в рабочей
    /// памяти остаётся несвёрнутым текстом.
    pub covers_through: EventSeq,
}

/// Собирает результат сворачивания из уже провалидированной сводки
/// (контракт `WorkingMemorySummary`, прошедший Mediation) и записей,
/// которые она заменяет. `entries` — история в порядке возрастания
/// `seq`; пустой срез сворачивать нечего — `None`, а не сводка без
/// смысла покрытия.
pub fn collapse(entries: &[HistoryEntry], summary: String) -> Option<CollapsedEntry> {
    let covers_through = entries.last()?.seq;
    Some(CollapsedEntry {
        summary,
        covers_through,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(seq: u64, text: &str) -> HistoryEntry {
        HistoryEntry {
            seq: EventSeq(seq),
            text: text.to_string(),
        }
    }

    #[test]
    fn total_chars_sums_entry_text_lengths() {
        let entries = [entry(1, "abc"), entry(2, "de")];
        assert_eq!(total_chars(&entries), 5);
    }

    #[test]
    fn total_chars_of_empty_history_is_zero() {
        assert_eq!(total_chars(&[]), 0);
    }

    #[test]
    fn exactly_at_budget_is_not_over_budget() {
        let entries = [entry(1, "12345")];
        assert!(!over_budget(&entries, 5));
    }

    #[test]
    fn one_char_over_budget_triggers_collapse() {
        let entries = [entry(1, "123456")];
        assert!(over_budget(&entries, 5));
    }

    #[test]
    fn collapse_of_empty_history_is_none() {
        assert!(collapse(&[], "сводка".into()).is_none());
    }

    #[test]
    fn collapse_uses_last_entrys_seq_as_the_covered_boundary() {
        let entries = [entry(1, "a"), entry(2, "b"), entry(5, "c")];
        let collapsed = collapse(&entries, "сводка трёх шагов".into()).unwrap();
        assert_eq!(collapsed.covers_through, EventSeq(5));
        assert_eq!(collapsed.summary, "сводка трёх шагов");
    }
}
