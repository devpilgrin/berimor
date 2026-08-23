//! Метер вызовов моделей (волна A, 0.38.0): обёртка над провайдером,
//! журналирующая usage (токены, латентность) с атрибуцией шага —
//! событие `ModelUsage` в журнал запуска. Цель журналирования
//! назначается ПОСЛЕ сборки бандла (instance id рождается при старте
//! прогона): до назначения метер молчит (конфиг-show, ревью и т.п.
//! вне прогона ничего не пишут).

use std::sync::{Arc, Mutex};
use std::time::Instant;

use berimor_types::event::{Event, EventKind, ProcessInstanceId};
use berimor_types::executor::ModelProvider;
use berimor_types::model::{CompletionRequest, CompletionResponse, ModelError};

/// Цель метра: журнал + instance id запуска. Общая для всех метров
/// бандла; назначается один раз при старте прогона.
pub type MeterTarget = Arc<Mutex<Option<(Arc<berimor_storage::SqliteEventLog>, String)>>>;

pub fn new_target() -> MeterTarget {
    Arc::new(Mutex::new(None))
}

pub struct MeteredProvider {
    inner: Arc<dyn ModelProvider + Send + Sync>,
    target: MeterTarget,
}

impl MeteredProvider {
    pub fn wrap(
        inner: Arc<dyn ModelProvider + Send + Sync>,
        target: MeterTarget,
    ) -> Arc<dyn ModelProvider + Send + Sync> {
        Arc::new(Self { inner, target })
    }
}

impl ModelProvider for MeteredProvider {
    fn complete(&self, request: CompletionRequest) -> Result<CompletionResponse, ModelError> {
        let step_id = request.step_id.clone();
        let started = Instant::now();
        let result = self.inner.complete(request);
        let latency_ms = started.elapsed().as_millis() as u64;
        if let Ok(response) = &result {
            let target = self.target.lock().expect("meter lock").clone();
            if let Some((journal, instance)) = target {
                let usage = response.usage;
                let event = Event::new(
                    ProcessInstanceId(instance),
                    1,
                    EventKind::ModelUsage {
                        step_id,
                        provider: response.model.provider.clone(),
                        model_id: response.model.model_id.clone(),
                        prompt_tokens: usage.map(|u| u.prompt_tokens).unwrap_or(0),
                        completion_tokens: usage.map(|u| u.completion_tokens).unwrap_or(0),
                        latency_ms,
                    },
                    serde_json::Value::Null,
                );
                crate::run::audit_append(journal.as_ref(), event);
            }
        }
        result
    }
}
