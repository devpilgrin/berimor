//! Failover между провайдерами одного класса (директива 2026-08-03:
//! «если несколько повторных вызовов не срабатывают — переключиться на
//! другую модель»). Транспортная недоступность (`ModelError::
//! Unavailable`) лучшего провайдера переводит запрос к следующему
//! кандидату из [`ModelPool::select_ranked`] — того же tier или
//! сильнее. Понижения класса нет: ранжирование пула это гарантирует,
//! а требование tier задаётся снизу.
//!
//! Ошибки НЕ-Unavailable (бюджет) failover не двигают — у них иная
//! природа, и «попробовать другого» там нерелевантно.

use std::sync::Arc;
use std::time::Duration;

pub use berimor_types::model::{
    BreakerRegistry, DEFAULT_BREAKER_COOLDOWN_SECS, DEFAULT_BREAKER_FAILURES,
};

use berimor_types::executor::ModelProvider;
use berimor_types::model::{CompletionRequest, CompletionResponse, ModelError};

/// Хук уведомления о переходе: (от провайдера, к провайдеру).
pub type ProviderSwitchHook<'a> = Option<&'a dyn Fn(&str, &str)>;

/// Обёртка над ранжированными кандидатами `(имя, провайдер)`: запрос
/// идёт лучшему, при Unavailable — следующему. О переключении сообщает
/// через хук (UI/телеметрия — пользователь ВИДИТ, какая модель ответила).
pub struct FailoverProvider<'a> {
    candidates: Vec<(&'a str, &'a dyn ModelProvider)>,
    /// (от провайдера, к провайдеру) — на каждом переходе.
    on_switch: ProviderSwitchHook<'a>,
    /// Circuit breaker (волна A): общий реестр на прогон; об открытии
    /// сообщаем через on_switch как переход «<имя> → circuit-open».
    breaker: Option<(Arc<BreakerRegistry>, u32, Duration)>,
}

impl<'a> FailoverProvider<'a> {
    pub fn new(
        candidates: Vec<(&'a str, &'a dyn ModelProvider)>,
        on_switch: ProviderSwitchHook<'a>,
    ) -> Self {
        Self {
            candidates,
            on_switch,
            breaker: None,
        }
    }

    /// Подключить автомат: общий реестр прогона, порог сбоев, cooldown.
    pub fn with_breaker(
        mut self,
        registry: Arc<BreakerRegistry>,
        threshold: u32,
        cooldown: Duration,
    ) -> Self {
        self.breaker = Some((registry, threshold, cooldown));
        self
    }
}

impl ModelProvider for FailoverProvider<'_> {
    fn complete(&self, request: CompletionRequest) -> Result<CompletionResponse, ModelError> {
        let mut last_err: Option<ModelError> = None;
        let mut previous_name: Option<&str> = None;
        let mut skipped: Vec<&str> = Vec::new();
        for (name, provider) in &self.candidates {
            // Автомат открыт — провайдер пропускаем без вызова.
            if let Some((registry, _, cooldown)) = &self.breaker {
                if !registry.is_available(name, *cooldown) {
                    skipped.push(name);
                    continue;
                }
            }
            match provider.complete(request.clone()) {
                Ok(response) => {
                    if let Some((registry, _, _)) = &self.breaker {
                        registry.record_success(name);
                    }
                    if let (Some(from), Some(hook)) = (previous_name, self.on_switch) {
                        hook(from, name);
                    }
                    return Ok(response);
                }
                Err(ModelError::Unavailable(err)) => {
                    if let Some((registry, threshold, _)) = &self.breaker {
                        if registry.record_failure(name, *threshold) {
                            // Алерт об открытии автомата — видимый
                            // пользователю переход «<имя> → circuit-open».
                            if let Some(hook) = self.on_switch {
                                hook(name, "circuit-open");
                            }
                        }
                    }
                    if let (Some(from), Some(hook)) = (previous_name, self.on_switch) {
                        hook(from, name);
                    }
                    previous_name = Some(name);
                    last_err = Some(ModelError::Unavailable(format!("{name}: {err}")));
                }
                // Не транспорт — у остальных будет то же.
                Err(other) => return Err(other),
            }
        }
        let mut message = last_err
            .map(|err| err.to_string())
            .unwrap_or_else(|| "нет кандидатов".to_string());
        if !skipped.is_empty() {
            message.push_str(&format!(
                "; пропущены по circuit breaker: {}",
                skipped.join(", ")
            ));
        }
        Err(ModelError::Unavailable(message))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use berimor_types::model::{ModelIdentity, ModelTier};

    struct Flaky {
        fails: bool,
        name: &'static str,
    }

    impl ModelProvider for Flaky {
        fn complete(&self, _request: CompletionRequest) -> Result<CompletionResponse, ModelError> {
            if self.fails {
                return Err(ModelError::Unavailable(format!(
                    "{}: сеть лежит",
                    self.name
                )));
            }
            Ok(CompletionResponse {
                raw_text: "ok".into(),
                model: ModelIdentity {
                    provider: self.name.into(),
                    model_id: "m".into(),
                    tier: ModelTier::Strong,
                },
                usage: None,
            })
        }
    }

    fn request() -> CompletionRequest {
        CompletionRequest {
            system_context: String::new(),
            prompt: "x".into(),
            contract_name: None,
            expects_structured_output: false,
            step_id: None,
            json_schema: None,
        }
    }

    #[test]
    fn falls_over_to_next_candidate_on_unavailable() {
        let down = Flaky {
            fails: true,
            name: "kimi",
        };
        let alive = Flaky {
            fails: false,
            name: "deepseek",
        };
        let failover = FailoverProvider::new(vec![("kimi", &down), ("deepseek", &alive)], None);
        let response = failover.complete(request()).unwrap();
        assert_eq!(response.model.provider, "deepseek");
    }

    #[test]
    fn all_down_reports_last_unavailable() {
        let a = Flaky {
            fails: true,
            name: "a",
        };
        let b = Flaky {
            fails: true,
            name: "b",
        };
        let failover = FailoverProvider::new(vec![("a", &a), ("b", &b)], None);
        let err = failover.complete(request()).unwrap_err();
        assert!(err.to_string().contains("b: b: сеть лежит"));
    }

    /// Circuit breaker (волна A): порог сбоев подряд → провайдер
    /// пропускается без вызова; успех соседа не трогает его автомат;
    /// сообщение называет пропущенных.
    #[test]
    fn breaker_opens_after_threshold_and_skips_provider() {
        let down = Flaky {
            fails: true,
            name: "kimi",
        };
        let alive = Flaky {
            fails: false,
            name: "deepseek",
        };
        let registry = BreakerRegistry::new();
        let make =
            || {
                FailoverProvider::new(vec![("kimi", &down), ("deepseek", &alive)], None)
                    .with_breaker(registry.clone(), 3, Duration::from_secs(120))
            };
        // Три сбоя kimi подряд (по вызову на запрос) → автомат открыт.
        for _ in 0..3 {
            let response = make().complete(request()).unwrap();
            assert_eq!(response.model.provider, "deepseek");
        }
        assert!(!registry.is_available("kimi", Duration::from_secs(120)));
        // Теперь kimi пропускается без вызова даже первым кандидатом:
        // вызываем только с одним кандидатом — получим ошибку с пометкой.
        let solo = FailoverProvider::new(vec![("kimi", &down)], None).with_breaker(
            registry,
            3,
            Duration::from_secs(120),
        );
        let err = solo.complete(request()).unwrap_err();
        assert!(err.to_string().contains("circuit breaker"));
    }

    /// Полуоткрытая проба: cooldown истёк — провайдер снова допускается;
    /// успех закрывает автомат.
    #[test]
    fn breaker_half_open_probe_after_cooldown() {
        let registry = BreakerRegistry::new();
        for _ in 0..3 {
            registry.record_failure("kimi", 3);
        }
        // cooldown = 0 — проба разрешена немедленно.
        assert!(registry.is_available("kimi", Duration::from_secs(0)));
        registry.record_success("kimi");
        assert!(registry.is_available("kimi", Duration::from_secs(3600)));
    }
}
