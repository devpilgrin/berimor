//! Семантическая память: контракт «предложение факта», дедупликация, гибридный поиск.
//!
//! Источник: `docs/arch/memory-model.md` §2–3, ADR-0005. ROADMAP: MEM3 (дедупликация) ·
//! MEM4 (sqlite-vec, гибридный поиск) · MEM5 (конфликт-события).
//!
//! MEM3 — только дедупликация: точное совпадение по хэшу решается здесь
//! детерминированно, без внешних зависимостей; близкое совпадение
//! («косинусная близость выше порога», §2) делегируется трейту
//! [`SimilaritySource`] — реальная реализация на эмбеддингах/`sqlite-vec`
//! появится в MEM4. Этот модуль не хранит факты сам (персистентность —
//! MEM4) и не решает конфликты противоречащих фактов (конфликт-события —
//! MEM5) — работает с уже загруженным вызывающим кодом срезом уже
//! существующих фактов, как `working::collapse` (MEM1) работает со
//! срезом уже загруженной истории.

use berimor_mediation::contracts::FactProposal;
use sha2::{Digest, Sha256};

/// Идентификатор факта в семантическом слое — назначается вызывающим
/// кодом при первой записи (не моделью, I1); дедупликация лишь сравнивает
/// новое предложение с уже существующими фактами, несущими такой id.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FactId(pub String);

/// Хэш нормализованной тройки «субъект-предикат-объект» — точное
/// совпадение по смыслу текста, не по байтам оригинального
/// регистра/пробелов вывода модели.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FactHash([u8; 32]);

fn fact_hash(subject: &str, predicate: &str, object: &str) -> FactHash {
    let normalized = format!(
        "{}\u{1}{}\u{1}{}",
        subject.trim().to_lowercase(),
        predicate.trim().to_lowercase(),
        object.trim().to_lowercase()
    );
    FactHash(Sha256::digest(normalized.as_bytes()).into())
}

/// Факт как он хранится в семантическом слое: предложение модели
/// (`FactProposal`) плюс метаданные, которые решает код, не модель.
/// `trusted_channel` — про КАНАЛ поступления факта (§2: «факты из
/// непроверенных внешних каналов получают пометку „ненадёжный
/// источник“»), а не про поле `source` — то, что модель заявила о
/// происхождении, не делает канал поступления доверенным автоматически.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredFact {
    pub id: FactId,
    pub subject: String,
    pub predicate: String,
    pub object: String,
    pub confidence: f32,
    pub source: String,
    pub trusted_channel: bool,
    hash: FactHash,
}

impl StoredFact {
    /// Собирает сохранённый факт из предложения, уже прошедшего
    /// дедупликацию (`DedupOutcome::New`). `id` и `trusted_channel`
    /// решает вызывающий код.
    pub fn new(id: FactId, proposal: &FactProposal, trusted_channel: bool) -> Self {
        Self {
            id,
            subject: proposal.subject.clone(),
            predicate: proposal.predicate.clone(),
            object: proposal.object.clone(),
            confidence: proposal.confidence,
            source: proposal.source.clone(),
            trusted_channel,
            hash: fact_hash(&proposal.subject, &proposal.predicate, &proposal.object),
        }
    }
}

/// Косинусная близость кандидата и существующего факта (§2: «близкое
/// (косинусная близость выше порога)»). Реальная реализация —
/// эмбеддинги/`sqlite-vec` (MEM4); здесь только граница интерфейса —
/// дедупликация не знает и не должна знать, как считается близость.
pub trait SimilaritySource {
    /// Значение в `[0.0, 1.0]`; `1.0` — совпадающий смысл.
    fn similarity(&self, candidate: &FactProposal, existing: &StoredFact) -> f32;
}

/// Похожести не существует (`similarity` всегда `0.0`) — для вызывающего
/// кода, которому пока негде взять эмбеддинги (до MEM4). Дедупликация
/// тогда работает только по точному хэшу — строго лучше, чем совсем без
/// дедупликации, не притворяется, что умеет больше, чем умеет.
pub struct NoSimilarity;

impl SimilaritySource for NoSimilarity {
    fn similarity(&self, _candidate: &FactProposal, _existing: &StoredFact) -> f32 {
        0.0
    }
}

/// Порог близости по умолчанию. `memory-model.md` §2 не называет
/// конкретное число — консервативная стартовая константа кода до
/// офлайн-калибровки (Фаза 9), как `context_engine::budget_chars` (C3).
pub const DEFAULT_SIMILARITY_THRESHOLD: f32 = 0.9;

#[derive(Debug, Clone, PartialEq)]
pub enum DedupOutcome {
    /// Точное совпадение — тот же факт уже есть, писать нечего.
    Duplicate {
        existing: FactId,
    },
    /// Близкое совпадение выше порога — слияние с существующим
    /// (`merge_confidence`), не отдельная запись.
    Merge {
        existing: FactId,
        similarity: f32,
    },
    New,
}

/// Решает судьбу предложенного факта относительно уже существующих —
/// дословно порядок §2: точное совпадение по хэшу → близкое совпадение
/// выше порога → новый факт. Точное совпадение проверяется первым и
/// безусловно: оно детерминировано и не зависит от качества
/// `similarity`, поэтому не может быть перекрыто близким совпадением.
pub fn dedup(
    candidate: &FactProposal,
    existing: &[StoredFact],
    similarity: &dyn SimilaritySource,
    threshold: f32,
) -> DedupOutcome {
    let candidate_hash = fact_hash(&candidate.subject, &candidate.predicate, &candidate.object);
    if let Some(exact) = existing.iter().find(|f| f.hash == candidate_hash) {
        return DedupOutcome::Duplicate {
            existing: exact.id.clone(),
        };
    }

    let best = existing
        .iter()
        .map(|f| (f, similarity.similarity(candidate, f)))
        .filter(|(_, score)| *score >= threshold)
        .max_by(|(_, a), (_, b)| a.total_cmp(b));

    match best {
        Some((fact, score)) => DedupOutcome::Merge {
            existing: fact.id.clone(),
            similarity: score,
        },
        None => DedupOutcome::New,
    }
}

/// Слияние уверенности: максимум из двух независимых наблюдений одного
/// факта — повторное наблюдение не должно СНИЖАТЬ уверенность. Текстовые
/// поля при слиянии не меняются (решает вызывающий код, не эта функция):
/// уже сохранённый факт канонический, смена формулировки при каждом
/// близком совпадении сделала бы факт нестабильным без обоснованной причины.
pub fn merge_confidence(existing_confidence: f32, candidate_confidence: f32) -> f32 {
    existing_confidence.max(candidate_confidence)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proposal(subject: &str, predicate: &str, object: &str, confidence: f32) -> FactProposal {
        FactProposal {
            subject: subject.into(),
            predicate: predicate.into(),
            object: object.into(),
            confidence,
            source: "session:run-1/step:answer".into(),
        }
    }

    fn stored(
        id: &str,
        subject: &str,
        predicate: &str,
        object: &str,
        confidence: f32,
    ) -> StoredFact {
        StoredFact::new(
            FactId(id.into()),
            &proposal(subject, predicate, object, confidence),
            true,
        )
    }

    struct FixedSimilarity(f32);
    impl SimilaritySource for FixedSimilarity {
        fn similarity(&self, _candidate: &FactProposal, _existing: &StoredFact) -> f32 {
            self.0
        }
    }

    #[test]
    fn empty_existing_facts_always_yields_new() {
        let candidate = proposal("клиент c-1", "предпочитает_канал", "email", 0.8);
        let outcome = dedup(&candidate, &[], &NoSimilarity, DEFAULT_SIMILARITY_THRESHOLD);
        assert_eq!(outcome, DedupOutcome::New);
    }

    #[test]
    fn exact_text_match_is_a_duplicate_regardless_of_similarity_source() {
        let existing = [stored(
            "f-1",
            "клиент c-1",
            "предпочитает_канал",
            "email",
            0.6,
        )];
        let candidate = proposal("клиент c-1", "предпочитает_канал", "email", 0.9);

        // FixedSimilarity(0.0) доказывает: точное совпадение находится
        // безусловно, не через similarity.
        let outcome = dedup(&candidate, &existing, &FixedSimilarity(0.0), 0.9);

        assert_eq!(
            outcome,
            DedupOutcome::Duplicate {
                existing: FactId("f-1".into())
            }
        );
    }

    #[test]
    fn exact_match_is_case_and_whitespace_insensitive() {
        let existing = [stored(
            "f-1",
            "Клиент C-1",
            "предпочитает_канал",
            "email",
            0.6,
        )];
        let candidate = proposal("  клиент c-1  ", "ПРЕДПОЧИТАЕТ_КАНАЛ", "Email", 0.9);

        let outcome = dedup(
            &candidate,
            &existing,
            &NoSimilarity,
            DEFAULT_SIMILARITY_THRESHOLD,
        );

        assert_eq!(
            outcome,
            DedupOutcome::Duplicate {
                existing: FactId("f-1".into())
            }
        );
    }

    #[test]
    fn near_duplicate_above_threshold_merges_with_the_best_match() {
        let existing = [
            stored("f-1", "клиент c-1", "живёт_в", "Москва", 0.5),
            stored("f-2", "клиент c-1", "город", "Москва город", 0.5),
        ];
        let candidate = proposal("клиент c-1", "проживает", "г. Москва", 0.7);

        // f-2 «более похож» по сценарию теста — max_by обязан выбрать его.
        struct ByFactId;
        impl SimilaritySource for ByFactId {
            fn similarity(&self, _candidate: &FactProposal, existing: &StoredFact) -> f32 {
                if existing.id == FactId("f-2".into()) {
                    0.95
                } else {
                    0.91
                }
            }
        }

        let outcome = dedup(&candidate, &existing, &ByFactId, 0.9);

        assert_eq!(
            outcome,
            DedupOutcome::Merge {
                existing: FactId("f-2".into()),
                similarity: 0.95
            }
        );
    }

    #[test]
    fn similarity_below_threshold_is_a_new_fact() {
        let existing = [stored("f-1", "клиент c-1", "живёт_в", "Москва", 0.5)];
        let candidate = proposal("клиент c-1", "любит", "чай", 0.7);

        let outcome = dedup(&candidate, &existing, &FixedSimilarity(0.5), 0.9);

        assert_eq!(outcome, DedupOutcome::New);
    }

    #[test]
    fn similarity_exactly_at_threshold_counts_as_a_match() {
        let existing = [stored("f-1", "клиент c-1", "живёт_в", "Москва", 0.5)];
        let candidate = proposal("клиент c-1", "проживает", "Москва-сити", 0.7);

        let outcome = dedup(&candidate, &existing, &FixedSimilarity(0.9), 0.9);

        assert!(matches!(outcome, DedupOutcome::Merge { .. }));
    }

    #[test]
    fn merge_confidence_never_decreases_on_reinforcement() {
        assert_eq!(merge_confidence(0.9, 0.4), 0.9);
        assert_eq!(merge_confidence(0.4, 0.9), 0.9);
    }

    #[test]
    fn stored_fact_carries_trusted_channel_flag_set_by_caller() {
        let untrusted = StoredFact::new(
            FactId("f-1".into()),
            &proposal("клиент c-1", "живёт_в", "Москва", 0.5),
            false,
        );
        assert!(!untrusted.trusted_channel);
    }
}
