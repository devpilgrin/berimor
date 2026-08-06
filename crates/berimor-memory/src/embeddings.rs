//! Провайдер эмбеддингов для семантической памяти (ROADMAP §20.23) —
//! реальная реализация шва `embed` в [`crate::semantic::VectorSimilarity`].
//!
//! Доступен только под feature `embeddings` (opt-in: ONNX Runtime и веса
//! модели не должны попадать в бинарник без явного флага). Модель —
//! `intfloat/multilingual-e5-small` ([`EmbeddingModel::MultilingualE5Small`]
//! в fastembed): мультиязычная (русский/английский в одном векторном
//! пространстве), 384-мерная, CPU-инференс через ONNX Runtime.
//!
//! Инициализация ЛЕНИВАЯ: конструктор не качает ~0.5 ГБ весов и не грузит
//! ONNX-сессию — только первый вызов [`FastEmbedder::embed`]. Это важно для
//! записного пути `berimor run`: эмбеддинги нужны лишь когда реально
//! извлеклись факты, а не на каждый запуск с включённой опцией.

use fastembed::{EmbeddingModel, TextEmbedding, TextInitOptions};
use std::path::PathBuf;
use std::sync::Mutex;

/// Размерность вектора multilingual-e5-small — константа контракта:
/// `sqlite-vec` таблица фактов создаётся под фиксированную размерность,
/// смена модели = смена этой константы и реиндексация.
pub const EMBEDDING_DIM: usize = 384;

/// Кэш весов модели — платформенный каталог данных пользователя плюс
/// `berimor/embeddings` (та же конвенция, что `plugin_install::
/// plugins_root_dir`: `~/.local/share/berimor/...` на Linux, fallback —
/// config_dir, затем временный каталог — деградация по постоянству между
/// запусками, не отказ).
pub fn default_cache_dir() -> PathBuf {
    dirs::data_dir()
        .or_else(dirs::config_dir)
        .map(|dir| dir.join("berimor").join("embeddings"))
        .unwrap_or_else(|| std::env::temp_dir().join("berimor-embeddings"))
}

/// Ленивый эмбеддер на fastembed. `embed` безопасен для повторных вызовов:
/// модель инициализируется один раз (Mutex — `TextEmbedding::embed` требует
/// `&mut self`, а шов `VectorSimilarity` принимает `Fn(&str)` с shared
/// ссылкой). Потокобезопасность здесь — не про параллелизм (записной путь
/// однопоточный), а про соответствие сигнатуре шва.
pub struct FastEmbedder {
    cache_dir: PathBuf,
    model: Mutex<Option<TextEmbedding>>,
}

impl FastEmbedder {
    /// Эмбеддер с кэшем в [`default_cache_dir`]. Не качает модель — см.
    /// документацию модуля про ленивость.
    pub fn new() -> Self {
        Self::with_cache_dir(default_cache_dir())
    }

    /// Эмбеддер с явным каталогом кэша — для тестов и нестандартных
    /// установок (тот же контракт `with_cache_dir`, что у
    /// `verify.rs::build_verifier` для sigstore-кэша).
    pub fn with_cache_dir(cache_dir: PathBuf) -> Self {
        Self {
            cache_dir,
            model: Mutex::new(None),
        }
    }

    /// Эмбеддинг текста — вектор размерности [`EMBEDDING_DIM`]. При первом
    /// вызове скачивает веса модели (~0.5 ГБ, huggingface) и инициализирует
    /// ONNX-сессию; ошибки сети/рантайма — ошибкой наверх, не пустым
    /// вектором (молчаливая подмена была бы ложной «непохожестью» в
    /// дедупликации — та же находка 4.7 аудита, что и у `SimilaritySource`).
    pub fn embed(&self, text: &str) -> Result<Vec<f32>, String> {
        let mut guard = self
            .model
            .lock()
            .map_err(|err| format!("мьютекс эмбеддера отравлен: {err}"))?;
        if guard.is_none() {
            let options = TextInitOptions::new(EmbeddingModel::MultilingualE5Small)
                .with_cache_dir(self.cache_dir.clone())
                .with_show_download_progress(false);
            let model = TextEmbedding::try_new(options)
                .map_err(|err| format!("инициализация fastembed: {err}"))?;
            *guard = Some(model);
        }
        let model = guard
            .as_mut()
            .expect("модель только что инициализирована выше");
        let mut embeddings = model
            .embed([text], None)
            .map_err(|err| format!("инференс эмбеддинга: {err}"))?;
        let embedding = embeddings
            .pop()
            .ok_or_else(|| "fastembed вернул пустой пакет эмбеддингов".to_string())?;
        if embedding.len() != EMBEDDING_DIM {
            return Err(format!(
                "неожиданная размерность эмбеддинга: {} (ожидалась {EMBEDDING_DIM})",
                embedding.len()
            ));
        }
        Ok(embedding)
    }
}

impl Default for FastEmbedder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Конструктор ленив: ни сети, ни ONNX-сессии до первого `embed` —
    /// проверяется без скачивания модели.
    #[test]
    fn constructor_is_lazy_and_does_not_touch_network() {
        let embedder = FastEmbedder::with_cache_dir(PathBuf::from("/nonexistent/cache"));
        assert!(embedder.model.lock().unwrap().is_none());
    }

    /// Конвенция каталога кэша — та же, что у плагинов: data_dir
    /// (`berimor/embeddings`), без отказа при недоступности платформенных
    /// каталогов (fallback на временный каталог).
    #[test]
    fn default_cache_dir_follows_plugin_convention() {
        let dir = default_cache_dir();
        assert!(
            dir.ends_with("berimor-embeddings") || dir.ends_with("berimor/embeddings"),
            "неожиданный каталог кэша: {}",
            dir.display()
        );
    }

    /// Контракт размерности зафиксирован — смена модели не должна пройти
    /// незамеченной (sqlite-vec таблица под 384).
    #[test]
    fn embedding_dim_matches_multilingual_e5_small() {
        assert_eq!(EMBEDDING_DIM, 384);
    }
}
