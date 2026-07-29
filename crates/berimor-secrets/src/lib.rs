//! `berimor-secrets` — тип-обёртка секрета и маскировщик на границе.
//!
//! Источник: `arch/security-model.md` §2 (L5), инвариант I4: «модель видит
//! только алиасы, никогда — значения». ROADMAP: F4, S5.

use std::fmt;

/// Значение никогда не реализует `Display`/`Debug` с утечкой — единственный
/// способ прочитать секрет — явный вызов [`Secret::reveal`] на границе, где
/// он действительно нужен (мост к хранилищу секретов), а не в логах,
/// подсказках модели или записи в память.
pub struct Secret(String);

impl Secret {
    pub fn new(value: String) -> Self {
        Self(value)
    }

    /// Явное раскрытие — используется только на границе моста к хранилищу
    /// секретов. Имя метода специально заметное — легко найти `grep`-ом все
    /// места, где секрет реально покидает обёртку.
    pub fn reveal(&self) -> &str {
        &self.0
    }

    /// То, что видит модель вместо значения (security-model.md §2, инвариант I4).
    pub fn alias(&self) -> String {
        "‹secret›".to_string()
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Secret(‹masked›)")
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "‹secret›")
    }
}
