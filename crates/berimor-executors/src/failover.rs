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
}

impl<'a> FailoverProvider<'a> {
    pub fn new(
        candidates: Vec<(&'a str, &'a dyn ModelProvider)>,
        on_switch: ProviderSwitchHook<'a>,
    ) -> Self {
        Self {
            candidates,
            on_switch,
        }
    }
}

impl ModelProvider for FailoverProvider<'_> {
    fn complete(&self, request: CompletionRequest) -> Result<CompletionResponse, ModelError> {
        let mut last_err: Option<ModelError> = None;
        let mut previous_name: Option<&str> = None;
        for (name, provider) in &self.candidates {
            match provider.complete(request.clone()) {
                Ok(response) => {
                    if let (Some(from), Some(hook)) = (previous_name, self.on_switch) {
                        hook(from, name);
                    }
                    return Ok(response);
                }
                Err(ModelError::Unavailable(err)) => {
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
        Err(last_err.unwrap_or_else(|| ModelError::Unavailable("нет кандидатов".into())))
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
            })
        }
    }

    fn request() -> CompletionRequest {
        CompletionRequest {
            system_context: String::new(),
            prompt: "x".into(),
            contract_name: None,
            expects_structured_output: false,
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
}
