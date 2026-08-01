//! `berimor-secrets` — тип-обёртка секрета и маскировщик на границах данных.
//!
//! Источник: `docs/arch/security-model.md` §2 (L5), инвариант I4: «модель видит
//! только алиасы, никогда — значения». ROADMAP: F4 (Secret), S5 (Masker).
//!
//! Два примитива:
//! - [`Secret`] — значение, недоступное через `Debug`/`Display`;
//! - [`Masker`] — реестр известных секретов ТЕКУЩЕГО ЗАПУСКА: заменяет их
//!   значения на алиас в текстах/JSON и отдаёт список значений для
//!   контроля утечек policy-стадии Mediation (`mediation.md` §4.3).

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

/// Минимальная длина регистрируемого значения. Короткое значение нельзя
/// ни маскировать, ни искать подстрокой без ложных срабатываний (токен
/// из 4 символов встретится в обычном тексте) — это осознанное
/// ограничение ТОЧНОСТИ детектора, не потеря секрета: значение и без
/// регистрации не выводится наружу нигде, кроме явного `reveal()`.
pub const MIN_SECRET_LEN: usize = 8;

/// Реестр известных секретов запуска — единая точка маскировки на всех
/// четырёх границах (`mediation.md` §4.3): аргументы/вывод инструментов,
/// мост к хранилищу, тексты подтверждений, контроль утечек в policy.
///
/// Реестр — источник истины о том, какие значения считаются секретами
/// ЭТОГО запуска; заполняется кодом сборки (CLI) из конфигурации и
/// окружения, никогда — из вывода модели (иначе модель решала бы, что
/// замаскировать, то есть что скрыть от проверок).
pub struct Masker {
    /// Значения по убыванию длины — при перекрытии (один секрет — префикс
    /// другого) сначала заменяется более длинный, иначе короткий оставил
    /// бы «хвост» длинного незамаскированным.
    values: Vec<Secret>,
}

impl Default for Masker {
    fn default() -> Self {
        Self::new()
    }
}

impl Masker {
    /// Пустой реестр: маскировка — no-op, список для контроля утечек —
    /// пуст (то же поведение, что было до S5; заполнение — обязанность
    /// сборки запуска). `const` — для статической пустой заглушки в тестах.
    pub const fn new() -> Self {
        Self { values: Vec::new() }
    }

    /// Регистрирует значение. Короткие (`< MIN_SECRET_LEN`) и пустые
    /// молча пропускаются — см. константу.
    pub fn register(&mut self, secret: Secret) {
        if secret.reveal().len() >= MIN_SECRET_LEN {
            self.values.push(secret);
            self.values
                .sort_by_key(|s| std::cmp::Reverse(s.reveal().len()));
        }
    }

    /// Мост к хранилищу: читает переменные окружения по ИМЕНАМ из
    /// конфигурации и регистрирует непустые значения. Имя переменной — не
    /// секрет; значение — секрет. Отсутствующая переменная — не ошибка
    /// (профиль может просто не иметь этого секрета).
    pub fn register_from_env(&mut self, var_names: &[String]) {
        for name in var_names {
            if let Ok(value) = std::env::var(name) {
                if !value.is_empty() {
                    self.register(Secret::new(value));
                }
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// Список известных значений — для `PolicyRules.known_secrets`
    /// (контроль утечек, точка 4). Вызывающий код обязан держать возвращённый
    /// вектор живым на всё время использования среза.
    pub fn known_values(&self) -> Vec<&str> {
        self.values.iter().map(|s| s.reveal()).collect()
    }

    /// Заменяет каждое известное значение в тексте на алиас. Точное
    /// совпадение подстрокой, без эвристик «похожести» — секрет либо
    /// буквально попал в текст, либо нет (тот же принцип, что в
    /// `policy::check_no_leaked_secrets`).
    pub fn mask_text(&self, text: &str) -> String {
        let mut out = text.to_string();
        for secret in &self.values {
            out = out.replace(secret.reveal(), &secret.alias());
        }
        out
    }

    /// Рекурсивная маскировка JSON: строки маскируются, числа/булевы/null
    /// проходят без изменений, структура объекта/массива сохраняется.
    pub fn mask_value(&self, value: &serde_json::Value) -> serde_json::Value {
        if self.is_empty() {
            return value.clone();
        }
        match value {
            serde_json::Value::String(s) => serde_json::Value::String(self.mask_text(s)),
            serde_json::Value::Object(map) => serde_json::Value::Object(
                map.iter()
                    .map(|(k, v)| (k.clone(), self.mask_value(v)))
                    .collect(),
            ),
            serde_json::Value::Array(items) => {
                serde_json::Value::Array(items.iter().map(|v| self.mask_value(v)).collect())
            }
            other => other.clone(),
        }
    }
}

impl fmt::Debug for Masker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Masker({} secrets)", self.values.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const FIXTURE: &str = include_str!("../../../fixtures/golden/security/secret-masking.json");

    #[derive(serde::Deserialize)]
    struct Fixture {
        secret: String,
        second_secret: String,
        alias: String,
        mask_cases: Vec<MaskCase>,
        json_cases: Vec<JsonCase>,
    }

    #[derive(serde::Deserialize)]
    struct MaskCase {
        name: String,
        input: String,
        expect_masked: bool,
    }

    #[derive(serde::Deserialize)]
    struct JsonCase {
        name: String,
        input: serde_json::Value,
        expect_masked: bool,
    }

    fn fixture_masker() -> (Masker, Fixture) {
        let fixture: Fixture = serde_json::from_str(FIXTURE).unwrap();
        let mut masker = Masker::new();
        masker.register(Secret::new(fixture.secret.clone()));
        masker.register(Secret::new(fixture.second_secret.clone()));
        (masker, fixture)
    }

    /// Контрактный тест на золотом наборе: каждый текстовый кейс
    /// маскируется ровно так, как задокументировано.
    #[test]
    fn golden_text_cases_are_masked_as_documented() {
        let (masker, fixture) = fixture_masker();
        for case in &fixture.mask_cases {
            let masked = masker.mask_text(&case.input);
            if case.expect_masked {
                assert!(
                    masked.contains(&fixture.alias),
                    "'{}': ожидался алиас в «{masked}»",
                    case.name
                );
                assert!(
                    !masked.contains(&fixture.secret),
                    "'{}': значение утекло в «{masked}»",
                    case.name
                );
            } else {
                assert_eq!(masked, case.input, "'{}': текст изменился", case.name);
            }
        }
    }

    #[test]
    fn golden_json_cases_are_masked_recursively() {
        let (masker, fixture) = fixture_masker();
        for case in &fixture.json_cases {
            let masked = masker.mask_value(&case.input);
            let as_text = masked.to_string();
            if case.expect_masked {
                assert!(!as_text.contains(&fixture.secret), "'{}'", case.name);
            } else {
                assert_eq!(masked, case.input, "'{}'", case.name);
            }
        }
    }

    #[test]
    fn longer_secret_wins_over_its_prefix() {
        let mut masker = Masker::new();
        masker.register(Secret::new("abcdefgh".into()));
        masker.register(Secret::new("abcdefgh-XYZW".into()));
        let masked = masker.mask_text("abcdefgh-XYZW");
        assert_eq!(
            masked, "‹secret›",
            "короткий префикс не должен оставлять хвост"
        );
    }

    #[test]
    fn short_values_are_not_registered() {
        let mut masker = Masker::new();
        masker.register(Secret::new("short".into()));
        assert!(masker.is_empty());
        assert_eq!(masker.mask_text("short"), "short");
    }

    #[test]
    fn debug_of_masker_does_not_leak_values() {
        let mut masker = Masker::new();
        masker.register(Secret::new("supersecretvalue".into()));
        let debug = format!("{masker:?}");
        assert!(!debug.contains("supersecretvalue"));
    }

    #[test]
    fn empty_masker_is_a_noop() {
        let masker = Masker::new();
        let value = json!({"key": "anything sk-test-FAKESECRET-9f8e7d6c"});
        assert_eq!(masker.mask_value(&value), value);
    }

    #[test]
    fn known_values_feeds_policy_leak_check() {
        let (masker, fixture) = fixture_masker();
        let known = masker.known_values();
        assert!(known.contains(&fixture.secret.as_str()));
        // Композиция с четвёртой точкой: та же policy-проверка, что зовёт
        // pipeline::mediate, обязана поймать значение из реестра.
        let output = json!({"reply": format!("ключ {}", fixture.secret)});
        assert!(
            berimor_mediation::policy::check_no_leaked_secrets(&output, &known).is_err(),
            "контроль утечек обязан сработать на значении из реестра"
        );
    }
}
