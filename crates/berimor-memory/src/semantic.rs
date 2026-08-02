//! Семантическая память: контракт «предложение факта», дедупликация, гибридный поиск.
//!
//! Источник: `docs/arch/memory-model.md` §2–3, ADR-0005. ROADMAP: MEM3 (дедупликация) ·
//! MEM4 (sqlite-vec, гибридный поиск) · MEM5 (конфликт-события).
//!
//! MEM3 — дедупликация: точное совпадение по хэшу решается здесь
//! детерминированно, без внешних зависимостей; близкое совпадение
//! («косинусная близость выше порога», §2) делегируется трейту
//! [`SimilaritySource`] — реальная реализация на эмбеддингах/`sqlite-vec`
//! появится в MEM4. MEM5 — конфликт-события: [`detect_conflict`] находит
//! факт с тем же субъектом/предикатом, но другим объектом, [`resolve`]
//! объединяет дедупликацию и обнаружение конфликта в одно решение с
//! правильным порядком проверок. Этот модуль не хранит факты сам
//! (персистентность — MEM4) и не решает конфликт САМ (кому и как
//! показать конфликт-событие — задача вызывающего кода/CLI-интеграции,
//! как и вызов модели в `pipeline::mediate`, M6) — работает со срезом уже
//! загруженных вызывающим кодом фактов, как `working::collapse` (MEM1)
//! работает со срезом уже загруженной истории.

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
pub struct FactHash([u8; 32]);

impl FactHash {
    /// Стабильная текстовая форма хэша — для идентификатора факта на
    /// записи (`f-<hex>` в записном пути `berimor run`).
    pub fn to_hex(&self) -> String {
        self.0.iter().map(|b| format!("{b:02x}")).collect()
    }
}

/// Нормализация текстового поля факта для сравнения по смыслу, не по
/// байтам: обрезка пробелов по краям + нижний регистр. Общая для точного
/// хэша ([`fact_hash`]) и структурного сравнения отношений
/// ([`detect_conflict`]) — оба должны считать один и тот же текст
/// «тем же самым» одинаково.
fn normalize(text: &str) -> String {
    text.trim().to_lowercase()
}

/// Детерминированный хэш содержимого факта — основа и дедупликации
/// (точное совпадение), и стабильного идентификатора на записи
/// (записной путь `berimor run` строит id как `f-<hash>`).
pub fn fact_hash(subject: &str, predicate: &str, object: &str) -> FactHash {
    let normalized = format!(
        "{}\u{1}{}\u{1}{}",
        normalize(subject),
        normalize(predicate),
        normalize(object)
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
    ///
    /// Маскировщик стоит НА ЗАПИСИ (memory-model.md §5: «секреты не живут
    /// в памяти», S5): текстовые поля проходят через `masker` до того, как
    /// станут хранимым фактом. Факт, содержавший значение секрета,
    /// сохраняется с алиасом вместо значения — чтение памяти моделью (I4)
    /// тогда безопасно по построению, без отдельной проверки на чтении.
    /// Хэш считается по УЖЕ замаскированным полям — дедупликация двух
    /// фактов с разными секретами на одном месте сольёт их (оба — алиас),
    /// что безопаснее обратного (размножения секретов по записям).
    pub fn new(
        id: FactId,
        proposal: &FactProposal,
        trusted_channel: bool,
        masker: &berimor_secrets::Masker,
    ) -> Self {
        let subject = masker.mask_text(&proposal.subject);
        let predicate = masker.mask_text(&proposal.predicate);
        let object = masker.mask_text(&proposal.object);
        Self {
            id,
            hash: fact_hash(&subject, &predicate, &object),
            subject,
            predicate,
            object,
            confidence: proposal.confidence,
            source: masker.mask_text(&proposal.source),
            trusted_channel,
        }
    }

    /// Восстанавливает StoredFact из записи хранилища (записной путь
    /// `berimor run`): hash пересобирается из полей — те же значения,
    /// что писались (замаскированные на записи), поэтому хэш совпадёт с
    /// сохранённым и дедупликация по точному совпадению работает.
    pub fn rehydrate(
        id: FactId,
        subject: String,
        predicate: String,
        object: String,
        confidence: f32,
        source: String,
        trusted_channel: bool,
    ) -> Self {
        let hash = fact_hash(&subject, &predicate, &object);
        Self {
            id,
            subject,
            predicate,
            object,
            confidence,
            source,
            trusted_channel,
            hash,
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

/// Реальная реализация [`SimilaritySource`] на `sqlite-vec` (MEM4,
/// `berimor_storage::SemanticStore::cosine_similarity`). Эмбеддинг
/// кандидата вычисляется через `embed` — этот тип не решает, КАК
/// получить эмбеддинг из текста (в системе нет провайдера эмбеддингов,
/// в ROADMAP нет такой задачи пока), только КАК его использовать, когда
/// он уже есть — та же граница ответственности, что и у самого трейта
/// `SimilaritySource` (MEM3): дедупликация не знает, откуда берётся
/// число близости, лишь то, что оно в `[0.0, 1.0]`.
///
/// `embed` вызывается по разу на каждый существующий факт в срезе
/// (`similarity` — метод без состояния между вызовами, кэшировать
/// эмбеддинг кандидата между ними тут негде без лишней сложности) — цена
/// оправдана, пока `embed` — не сетевой вызов; когда появится реальный
/// провайдер эмбеддингов, кэширование — задача этой самой интеграции,
/// не заранее выдуманная здесь абстракция.
pub struct VectorSimilarity<'a> {
    pub store: &'a dyn berimor_storage::SemanticStore,
    pub embed: &'a dyn Fn(&str) -> Vec<f32>,
}

impl SimilaritySource for VectorSimilarity<'_> {
    fn similarity(&self, candidate: &FactProposal, existing: &StoredFact) -> f32 {
        let text = format!(
            "{} {} {}",
            candidate.subject, candidate.predicate, candidate.object
        );
        let embedding = (self.embed)(&text);
        self.store
            .cosine_similarity(&existing.id.0, &embedding)
            .ok()
            .flatten()
            .unwrap_or(0.0)
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

fn dedup_exact(candidate: &FactProposal, existing: &[StoredFact]) -> Option<FactId> {
    let candidate_hash = fact_hash(&candidate.subject, &candidate.predicate, &candidate.object);
    existing
        .iter()
        .find(|f| f.hash == candidate_hash)
        .map(|f| f.id.clone())
}

fn dedup_near(
    candidate: &FactProposal,
    existing: &[StoredFact],
    similarity: &dyn SimilaritySource,
    threshold: f32,
) -> Option<(FactId, f32)> {
    existing
        .iter()
        .map(|f| (f, similarity.similarity(candidate, f)))
        .filter(|(_, score)| *score >= threshold)
        .max_by(|(_, a), (_, b)| a.total_cmp(b))
        .map(|(f, score)| (f.id.clone(), score))
}

/// Решает судьбу предложенного факта относительно уже существующих —
/// дословно порядок §2: точное совпадение по хэшу → близкое совпадение
/// выше порога → новый факт. Точное совпадение проверяется первым и
/// безусловно: оно детерминировано и не зависит от качества
/// `similarity`, поэтому не может быть перекрыто близким совпадением.
///
/// Не обнаруживает противоречия (MEM5, [`detect_conflict`]) — вызывающий
/// код, которому нужна защита и от дублей, и от противоречий разом,
/// использует [`resolve`], не эту функцию напрямую.
pub fn dedup(
    candidate: &FactProposal,
    existing: &[StoredFact],
    similarity: &dyn SimilaritySource,
    threshold: f32,
) -> DedupOutcome {
    if let Some(id) = dedup_exact(candidate, existing) {
        return DedupOutcome::Duplicate { existing: id };
    }
    match dedup_near(candidate, existing, similarity, threshold) {
        Some((id, score)) => DedupOutcome::Merge {
            existing: id,
            similarity: score,
        },
        None => DedupOutcome::New,
    }
}

/// Факт, структурно противоречащий кандидату (§2: «Противоречие нового
/// факта существующему» — инвариант I2, «не молчаливая перезапись»).
/// ROADMAP: MEM5.
#[derive(Debug, Clone, PartialEq)]
pub struct FactConflict {
    pub existing: FactId,
    pub existing_object: String,
    pub candidate_object: String,
}

/// Ищет факт с тем же субъектом и предикатом, что и кандидат, но другим
/// объектом — то же отношение утверждает разные вещи. Проверяется по
/// нормализованному ТЕКСТУ (`normalize`), не через `SimilaritySource`:
/// доверять оценке близости здесь нельзя — эмбеддинг-модель вполне может
/// счесть два взаимоисключающих утверждения «похожими» (общая тема,
/// противоположное значение — «клиент живёт в Москве» и «клиент живёт в
/// Париже» лексически близки), тогда как совпадение субъекта и
/// предиката — надёжный структурный сигнал: это одно и то же отношение,
/// значит два разных объекта не могут быть оба верны одновременно.
///
/// Возвращает первое найденное противоречие, не все — одного достаточно,
/// чтобы не писать кандидата молча; остальные всплывут при следующей
/// попытке записи после того, как человек разрешит это противоречие.
pub fn detect_conflict(candidate: &FactProposal, existing: &[StoredFact]) -> Option<FactConflict> {
    let subject = normalize(&candidate.subject);
    let predicate = normalize(&candidate.predicate);
    let object = normalize(&candidate.object);
    existing.iter().find_map(|fact| {
        let same_relation =
            normalize(&fact.subject) == subject && normalize(&fact.predicate) == predicate;
        let different_object = normalize(&fact.object) != object;
        if same_relation && different_object {
            Some(FactConflict {
                existing: fact.id.clone(),
                existing_object: fact.object.clone(),
                candidate_object: candidate.object.clone(),
            })
        } else {
            None
        }
    })
}

/// Полное решение по предложенному факту — MEM3 (дедупликация) и MEM5
/// (конфликт) вместе.
#[derive(Debug, Clone, PartialEq)]
pub enum Resolution {
    Duplicate {
        existing: FactId,
    },
    /// Между точным совпадением и близким — раньше близкого совпадения:
    /// см. [`detect_conflict`] о том, почему `similarity` не годится для
    /// обнаружения противоречий (могла бы ошибочно классифицировать
    /// конфликт как `Merge`, если модель близости сочла тему похожей).
    Conflict(FactConflict),
    Merge {
        existing: FactId,
        similarity: f32,
    },
    New,
}

/// Точный порядок §2 целиком, с врезанной проверкой противоречия между
/// точным и близким совпадением (см. [`Resolution::Conflict`]).
pub fn resolve(
    candidate: &FactProposal,
    existing: &[StoredFact],
    similarity: &dyn SimilaritySource,
    threshold: f32,
) -> Resolution {
    if let Some(id) = dedup_exact(candidate, existing) {
        return Resolution::Duplicate { existing: id };
    }
    if let Some(conflict) = detect_conflict(candidate, existing) {
        return Resolution::Conflict(conflict);
    }
    match dedup_near(candidate, existing, similarity, threshold) {
        Some((id, score)) => Resolution::Merge {
            existing: id,
            similarity: score,
        },
        None => Resolution::New,
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
        // Пустой реестр — маскировка no-op; существующие тесты не про S5.
        StoredFact::new(
            FactId(id.into()),
            &proposal(subject, predicate, object, confidence),
            true,
            &berimor_secrets::Masker::new(),
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

    /// S5, memory-model.md §5: маскировщик стоит на записи — значение
    /// секрета из реестра не попадает в хранимый факт, только алиас.
    #[test]
    fn write_masks_registered_secret_values() {
        let mut masker = berimor_secrets::Masker::new();
        masker.register(berimor_secrets::Secret::new(
            "sk-test-FAKESECRET-9f8e7d6c".into(),
        ));
        let fact = StoredFact::new(
            FactId("f-2".into()),
            &proposal(
                "сервис billing",
                "использует ключ",
                "sk-test-FAKESECRET-9f8e7d6c",
                0.9,
            ),
            true,
            &masker,
        );

        assert_eq!(fact.object, "‹secret›");
        assert!(!fact.subject.contains("sk-test"));
        assert!(!fact.source.contains("sk-test"));
    }

    #[test]
    fn stored_fact_carries_trusted_channel_flag_set_by_caller() {
        let untrusted = StoredFact::new(
            FactId("f-1".into()),
            &proposal("клиент c-1", "живёт_в", "Москва", 0.5),
            false,
            &berimor_secrets::Masker::new(),
        );
        assert!(!untrusted.trusted_channel);
    }

    #[test]
    fn detect_conflict_finds_same_relation_with_different_object() {
        let existing = [stored("f-1", "клиент c-1", "живёт_в", "Москва", 0.6)];
        let candidate = proposal("клиент c-1", "живёт_в", "Париж", 0.7);

        let conflict = detect_conflict(&candidate, &existing).unwrap();

        assert_eq!(conflict.existing, FactId("f-1".into()));
        assert_eq!(conflict.existing_object, "Москва");
        assert_eq!(conflict.candidate_object, "Париж");
    }

    #[test]
    fn detect_conflict_is_case_and_whitespace_insensitive_on_the_relation() {
        let existing = [stored("f-1", "Клиент C-1", "ЖИВЁТ_В", "Москва", 0.6)];
        let candidate = proposal("  клиент c-1  ", "живёт_в", "Париж", 0.7);

        assert!(detect_conflict(&candidate, &existing).is_some());
    }

    #[test]
    fn detect_conflict_none_when_object_matches() {
        // Тот же объект — не противоречие, это дело dedup (Duplicate/Merge).
        let existing = [stored("f-1", "клиент c-1", "живёт_в", "Москва", 0.6)];
        let candidate = proposal("клиент c-1", "живёт_в", "Москва", 0.7);

        assert!(detect_conflict(&candidate, &existing).is_none());
    }

    #[test]
    fn detect_conflict_none_when_relation_differs() {
        let existing = [stored("f-1", "клиент c-1", "живёт_в", "Москва", 0.6)];
        // Другой предикат — не то же отношение, значит не противоречие.
        let candidate = proposal("клиент c-1", "работает_в", "Москва", 0.7);

        assert!(detect_conflict(&candidate, &existing).is_none());
    }

    #[test]
    fn resolve_reports_conflict_even_when_similarity_would_have_suggested_merge() {
        let existing = [stored("f-1", "клиент c-1", "живёт_в", "Москва", 0.6)];
        let candidate = proposal("клиент c-1", "живёт_в", "Париж", 0.7);

        // FixedSimilarity(1.0) доказывает: будь порядок «similarity раньше
        // конфликта», это ошибочно стало бы Merge — resolve обязан
        // отдать Conflict первым.
        let outcome = resolve(&candidate, &existing, &FixedSimilarity(1.0), 0.9);

        assert_eq!(
            outcome,
            Resolution::Conflict(FactConflict {
                existing: FactId("f-1".into()),
                existing_object: "Москва".into(),
                candidate_object: "Париж".into(),
            })
        );
    }

    #[test]
    fn resolve_matches_dedup_for_exact_and_merge_cases() {
        let existing = [stored("f-1", "клиент c-1", "живёт_в", "Москва", 0.6)];

        let exact = proposal("клиент c-1", "живёт_в", "Москва", 0.9);
        assert_eq!(
            resolve(&exact, &existing, &NoSimilarity, 0.9),
            Resolution::Duplicate {
                existing: FactId("f-1".into())
            }
        );

        // Другой субъект — не конфликт (другое отношение вовсе), близкое
        // совпадение проходит как обычно.
        let unrelated_but_similar = proposal("клиент c-2", "живёт_в", "Питер", 0.7);
        assert_eq!(
            resolve(
                &unrelated_but_similar,
                &existing,
                &FixedSimilarity(0.95),
                0.9
            ),
            Resolution::Merge {
                existing: FactId("f-1".into()),
                similarity: 0.95
            }
        );
    }

    #[test]
    fn resolve_is_new_when_nothing_matches_or_conflicts() {
        let existing = [stored("f-1", "клиент c-1", "живёт_в", "Москва", 0.6)];
        let candidate = proposal("клиент c-1", "любит", "чай", 0.7);

        let outcome = resolve(&candidate, &existing, &FixedSimilarity(0.1), 0.9);

        assert_eq!(outcome, Resolution::New);
    }

    /// Композиция MEM3+MEM4: `VectorSimilarity` через реальный
    /// `sqlite-vec` (`berimor-storage`), не через фейк близости — та же
    /// дисциплина, что и `tool_only.rs` (E1) на golden-фикстуре P1:
    /// проверять реальную интеграцию, не только изолированную логику.
    #[test]
    fn resolve_with_real_sqlite_vec_similarity_merges_close_facts() {
        use berimor_storage::{FactRecord, SemanticStore, SqliteEventLog};

        let store = SqliteEventLog::open_in_memory().unwrap();
        store
            .upsert_fact(
                &FactRecord {
                    id: "f-1".into(),
                    subject: "клиент c-1".into(),
                    predicate: "живёт_в".into(),
                    object: "Москва".into(),
                    confidence: 0.6,
                    source: "session:run-1/step:answer".into(),
                    trusted_channel: true,
                },
                Some(&[1.0, 0.0, 0.0]),
            )
            .unwrap();

        // Фейковый эмбеддер: детерминированно возвращает тот же вектор,
        // что уже сохранён у f-1, — этого достаточно, чтобы доказать, что
        // similarity() реально идёт через SQL-вызов sqlite-vec, а не
        // просто возвращает константу.
        let embed = |_text: &str| vec![1.0f32, 0.0, 0.0];
        let similarity = VectorSimilarity {
            store: &store,
            embed: &embed,
        };
        let existing = [stored("f-1", "клиент c-1", "живёт_в", "Москва", 0.6)];
        let candidate = proposal("клиент c-1", "проживает", "г. Москва", 0.7);

        let outcome = resolve(&candidate, &existing, &similarity, 0.9);

        assert_eq!(
            outcome,
            Resolution::Merge {
                existing: FactId("f-1".into()),
                similarity: 1.0,
            }
        );
    }

    #[test]
    fn vector_similarity_falls_back_to_zero_for_unknown_fact() {
        use berimor_storage::SqliteEventLog;

        let store = SqliteEventLog::open_in_memory().unwrap();
        let embed = |_text: &str| vec![1.0f32, 0.0, 0.0];
        let similarity = VectorSimilarity {
            store: &store,
            embed: &embed,
        };
        let candidate = proposal("клиент c-1", "живёт_в", "Москва", 0.7);
        let unknown = stored("f-missing", "клиент c-1", "живёт_в", "Москва", 0.6);

        assert_eq!(similarity.similarity(&candidate, &unknown), 0.0);
    }
}
