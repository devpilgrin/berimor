//! `berimor-model-pool` — реестр моделей, классы, селектор провайдера.
//!
//! Источник: `docs/arch/ideal-agent-architecture.md` §3.10, ADR-0010
//! (класс — поле реестра, не самооценка), ADR-0011 (выбор провайдера —
//! детерминированное правило). ROADMAP: E3 (реестр/селектор) · E4
//! (llama.cpp) · E5 (удалённые провайдеры).
//!
//! Селектор — код, не вопрос к модели (I1). Правило выбора внутри класса,
//! дословно ADR-0011: предпочтение локального инференса при равном классе →
//! наименьшая стоимость среди удалённых в пределах латентность-бюджета →
//! фиксированный порядок предпочтения в конфигурации (порядок регистрации).
//!
//! Сознательно НЕ реализовано здесь (не требуется Milestone 1,
//! `docs/ROADMAP.md` §18.2): проверка здоровья и события деградации
//! (это контур офлайн-оценки, Фаза 9), цепочки отказоустойчивого
//! переключения (нужны, когда провайдеров больше одного реального).

use berimor_types::model::{ModelIdentity, ModelTier, ModelTierRequirement};

pub mod http_provider;
pub mod local_provider;

/// Природа провайдера: локальный инференс (llama.cpp, E4) или удалённый
/// (HTTP, E5). Локальный предпочтителен при равном классе — нулевая
/// предельная стоимость, данные не покидают периметр (ADR-0011).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderKind {
    Local,
    Remote,
}

/// Запись реестра моделей. Класс присваивается при регистрации из паспорта
/// модели, дальше переопределяется только офлайн-оценкой (ADR-0010) —
/// здесь это просто поле, механизм переоценки — Фаза 9.
#[derive(Debug, Clone)]
pub struct ModelEntry {
    pub identity: ModelIdentity,
    pub kind: ProviderKind,
    /// Стоимость из прайс-таблицы реестра — код-данные, «не то, что
    /// сообщает о своей стоимости сама модель» (ideal-agent §3.10).
    /// `None` — локальный инференс: предельная стоимость нулевая.
    pub cost_per_1k_tokens: Option<f64>,
    /// ИЗМЕРЕННАЯ латентность за скользящее окно, не заявленная
    /// (ideal-agent §3.10). `None` — измерений ещё не было; неизвестность
    /// не исключает провайдера из выбора.
    pub measured_latency_ms: Option<u64>,
}

/// Реестр моделей + селектор. Порядок регистрации значим: это и есть
/// «явный фиксированный порядок предпочтения в конфигурации» (ADR-0011) —
/// финальный критерий, когда предыдущие равны.
#[derive(Default, Clone)]
pub struct ModelPool {
    entries: Vec<ModelEntry>,
}

impl ModelPool {
    pub fn new() -> Self {
        Self::default()
    }

    /// Регистрация — код, вызываемый из конфигурации; класс берётся из
    /// паспорта модели, а не запрашивается у неё (ADR-0010).
    pub fn register(&mut self, entry: ModelEntry) {
        self.entries.push(entry);
    }

    pub fn entries(&self) -> &[ModelEntry] {
        &self.entries
    }

    /// Выбор провайдера под требование шага. Возвращает `None`, если ни
    /// одна запись не удовлетворяет классу и бюджету — отсутствие
    /// провайдера не повод молча понизить класс шага (ideal-agent §3.10:
    /// «не молчаливое понижение»).
    pub fn select(
        &self,
        requirement: ModelTierRequirement,
        latency_budget_ms: Option<u64>,
    ) -> Option<&ModelEntry> {
        self.select_ranked(requirement, latency_budget_ms)
            .into_iter()
            .next()
    }

    /// Упорядоченный список кандидатов (тот же порядок, что у
    /// [`select`]) — основа failover: недоступность лучшего провайдера
    /// — переход к следующему ТОГО ЖЕ класса или сильнее, не молчаливое
    /// понижение (ideal-agent §3.10, директива 2026-08-03).
    pub fn select_ranked(
        &self,
        requirement: ModelTierRequirement,
        latency_budget_ms: Option<u64>,
    ) -> Vec<&ModelEntry> {
        let min_tier = match requirement {
            ModelTierRequirement::Any => ModelTier::Weak,
            ModelTierRequirement::Weak => ModelTier::Weak,
            ModelTierRequirement::Medium => ModelTier::Medium,
            ModelTierRequirement::Strong => ModelTier::Strong,
        };

        let mut candidates: Vec<(usize, &ModelEntry)> = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| e.identity.tier >= min_tier)
            // Исключаются только провайдеры, ИЗМЕРЕННАЯ латентность которых
            // заведомо превышает бюджет шага; без измерений — не исключаем.
            .filter(|(_, e)| match (latency_budget_ms, e.measured_latency_ms) {
                (Some(budget), Some(measured)) => measured <= budget,
                _ => true,
            })
            .collect::<Vec<_>>();
        candidates.sort_by_key(|(registration_order, e)| {
            (
                // 1. Локальный при равном классе — впереди.
                matches!(e.kind, ProviderKind::Remote),
                // 2. Наименьшая стоимость среди оставшихся; None
                //    (локальный) дешевле любой цены — но до сюда
                //    локальный доходит только если уже отсортирован.
                e.cost_per_1k_tokens
                    .map(|c| (c * 1_000_000.0) as u64)
                    .unwrap_or(0),
                // 3. Фиксированный порядок конфигурации.
                *registration_order,
            )
        });
        candidates.into_iter().map(|(_, e)| e).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(
        provider: &str,
        tier: ModelTier,
        kind: ProviderKind,
        cost: Option<f64>,
        latency_ms: Option<u64>,
    ) -> ModelEntry {
        ModelEntry {
            identity: ModelIdentity {
                provider: provider.into(),
                model_id: format!("{provider}-model"),
                tier,
            },
            kind,
            cost_per_1k_tokens: cost,
            measured_latency_ms: latency_ms,
        }
    }

    #[test]
    fn empty_pool_selects_nothing() {
        let pool = ModelPool::new();
        assert!(pool.select(ModelTierRequirement::Any, None).is_none());
    }

    #[test]
    fn requirement_filters_weaker_tiers() {
        let mut pool = ModelPool::new();
        pool.register(entry(
            "local-weak",
            ModelTier::Weak,
            ProviderKind::Local,
            None,
            None,
        ));
        pool.register(entry(
            "remote-strong",
            ModelTier::Strong,
            ProviderKind::Remote,
            Some(5.0),
            None,
        ));

        let selected = pool.select(ModelTierRequirement::Strong, None).unwrap();
        assert_eq!(selected.identity.provider, "remote-strong");
    }

    #[test]
    fn local_is_preferred_over_cheaper_remote_at_same_tier() {
        let mut pool = ModelPool::new();
        // Регистрируем удалённый первым — порядок регистрации не должен
        // побеждать правило «локальный при равном классе».
        pool.register(entry(
            "remote-cheap",
            ModelTier::Medium,
            ProviderKind::Remote,
            Some(0.01),
            None,
        ));
        pool.register(entry(
            "local-medium",
            ModelTier::Medium,
            ProviderKind::Local,
            None,
            None,
        ));

        let selected = pool.select(ModelTierRequirement::Medium, None).unwrap();
        assert_eq!(selected.identity.provider, "local-medium");
    }

    #[test]
    fn cheapest_remote_wins_among_remotes() {
        let mut pool = ModelPool::new();
        pool.register(entry(
            "remote-expensive",
            ModelTier::Strong,
            ProviderKind::Remote,
            Some(10.0),
            None,
        ));
        pool.register(entry(
            "remote-cheap",
            ModelTier::Strong,
            ProviderKind::Remote,
            Some(2.0),
            None,
        ));

        let selected = pool.select(ModelTierRequirement::Strong, None).unwrap();
        assert_eq!(selected.identity.provider, "remote-cheap");
    }

    #[test]
    fn equal_cost_falls_back_to_registration_order() {
        let mut pool = ModelPool::new();
        pool.register(entry(
            "first",
            ModelTier::Weak,
            ProviderKind::Remote,
            Some(1.0),
            None,
        ));
        pool.register(entry(
            "second",
            ModelTier::Weak,
            ProviderKind::Remote,
            Some(1.0),
            None,
        ));

        let selected = pool.select(ModelTierRequirement::Any, None).unwrap();
        assert_eq!(selected.identity.provider, "first");
    }

    #[test]
    fn measured_latency_above_budget_excludes_provider() {
        let mut pool = ModelPool::new();
        pool.register(entry(
            "slow",
            ModelTier::Medium,
            ProviderKind::Remote,
            Some(0.5),
            Some(9_000),
        ));
        pool.register(entry(
            "fast",
            ModelTier::Medium,
            ProviderKind::Remote,
            Some(1.0),
            Some(500),
        ));

        let selected = pool
            .select(ModelTierRequirement::Medium, Some(1_000))
            .unwrap();
        assert_eq!(selected.identity.provider, "fast");
    }

    #[test]
    fn unmeasured_latency_is_not_excluded() {
        let mut pool = ModelPool::new();
        pool.register(entry(
            "unmeasured",
            ModelTier::Medium,
            ProviderKind::Remote,
            Some(1.0),
            None,
        ));

        let selected = pool.select(ModelTierRequirement::Medium, Some(1)).unwrap();
        assert_eq!(selected.identity.provider, "unmeasured");
    }

    #[test]
    fn no_matching_provider_is_none_not_silent_downgrade() {
        let mut pool = ModelPool::new();
        pool.register(entry(
            "weak",
            ModelTier::Weak,
            ProviderKind::Local,
            None,
            None,
        ));

        assert!(pool.select(ModelTierRequirement::Strong, None).is_none());
        // И при непосильном латентность-бюджете.
        let mut pool2 = ModelPool::new();
        pool2.register(entry(
            "slow",
            ModelTier::Weak,
            ProviderKind::Remote,
            Some(1.0),
            Some(60_000),
        ));
        assert!(pool2.select(ModelTierRequirement::Any, Some(100)).is_none());
    }
}
