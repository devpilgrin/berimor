//! Интеграционный тест РЕАЛЬНОЙ модели эмбеддингов (ROADMAP §20.23).
//!
//! ВНИМАНИЕ: при первом запуске скачивает ~0.5 ГБ весов
//! (`intfloat/multilingual-e5-small` с huggingface) в пользовательский
//! кэш (`~/.local/share/berimor/embeddings`). В CI не запускается —
//! только вручную, локально:
//!
//! ```sh
//! cargo test -p berimor-memory --features embeddings --test embeddings_real -- --include-ignored
//! ```

#![cfg(feature = "embeddings")]

use berimor_memory::embeddings::{FastEmbedder, EMBEDDING_DIM};

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    dot / (na * nb)
}

/// Скачивает модель (~0.5 ГБ на первом запуске). Проверяет контракт,
/// на который рассчитывает дедупликация семантической памяти:
/// перифразы одного факта (в т.ч. русский ↔ английский — рабочие языки
/// агента) дают косинус > 0.8 (выше порога слияния 0.9? нет — но близко
/// к нему и уверенно выше «случайного»), несвязанные факты < 0.5,
/// размерность ровно 384.
#[test]
#[ignore = "скачивает ~0.5 ГБ весов модели; запуск: --include-ignored"]
fn multilingual_e5_small_scores_paraphrases_high_and_unrelated_low() {
    // Дефолт (BGE-M3 после калибровки 2026-08-06: разрыв 0.275 против
    // 0.003 у e5-small и 0.045 у e5-base — таблица в ROADMAP §20.23).
    let embedder = FastEmbedder::new();

    // Перифразы: русский ↔ русский и русский ↔ английский.
    let paraphrase_pairs = [
        ("Клиент живёт в Москве", "Клиент проживает в городе Москва"),
        (
            "The client prefers email",
            "Клиент предпочитает общаться по почте",
        ),
        (
            "Проект написан на Rust",
            "The project is implemented in Rust",
        ),
    ];
    // Несвязанные пары.
    let unrelated_pairs = [
        ("Клиент живёт в Москве", "Квантовая запутанность фотонов"),
        ("Проект написан на Rust", "Рецепт борща на четыре порции"),
        (
            "The client prefers email",
            "Миграция базы данных запланирована на пятницу",
        ),
    ];

    let mut dims = Vec::new();
    for (a, b) in paraphrase_pairs.iter().chain(unrelated_pairs.iter()) {
        for text in [a, b] {
            let embedding = embedder.embed(text).expect("инференс эмбеддинга");
            assert_eq!(
                embedding.len(),
                EMBEDDING_DIM,
                "размерность вектора для «{text}»"
            );
            dims.push(embedding);
        }
    }

    for (i, (a, b)) in paraphrase_pairs.iter().enumerate() {
        let score = cosine(&dims[i * 2], &dims[i * 2 + 1]);
        eprintln!("PARA {i}: {score:.4} «{a}» ↔ «{b}»");
        assert!(
            score > berimor_memory::embeddings::BGE_M3_MERGE_THRESHOLD,
            "перифразы «{a}» ↔ «{b}» дали косинус {score:.3} (обязано превышать калиброванный порог слияния)"
        );
    }
    let base = paraphrase_pairs.len() * 2;
    for (i, (a, b)) in unrelated_pairs.iter().enumerate() {
        let score = cosine(&dims[base + i * 2], &dims[base + i * 2 + 1]);
        eprintln!("UNREL {i}: {score:.4} «{a}» ↔ «{b}»");
        assert!(
            score < berimor_memory::embeddings::BGE_M3_MERGE_THRESHOLD,
            "несвязанные «{a}» ↔ «{b}» дали косинус {score:.3} (обязано быть ниже калиброванного порога слияния)"
        );
    }
}
