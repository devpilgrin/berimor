//! StructuredLLM — узкий шаг с моделью: извлечь/классифицировать/суммаризировать.
//!
//! Источник: `docs/arch/executors.md` §3, `docs/arch/mediation.md` §5.
//! ROADMAP: E2 (+ интеграция E2+M6: цикл повтора с текстом ошибки в
//! подсказке — тот самый, что `pipeline.rs` оставлял «задачей интеграции»).
//!
//! Цепочка — дословно `docs/ROADMAP.md` §18.3 п.5: подсказка из
//! `Contract::json_schema()` + пример той же версии + срез контекста
//! (C1–C3) → `ModelPool::select` (E3) → `ModelProvider::complete` (E5) →
//! `berimor_mediation::pipeline::mediate` (M1–M7) → `Patch`. Исполнитель
//! не пишет в состояние сам — возвращает патч движку (`executors.md` §7).

use berimor_context_engine::ContextBuilder;
use berimor_mediation::{
    commit::CommitOutcome,
    contracts::{ChatReply, ClassificationOut, FactProposalBatch, SupportReply},
    pipeline::{self, PolicyRules},
    policy,
};
use berimor_model_pool::ModelPool;
use berimor_types::{
    contract::Contract,
    executor::ModelProvider,
    mediation::{MediationOutcome, MediationStage},
    model::{CompletionRequest, ModelError, ModelTier, ModelTierRequirement},
    step::Patch,
};
use serde_json::Value;
use std::collections::HashMap;

/// До 2 повторов на parse/schema — mediation.md §5 (та же константа, что
/// лимитирует `pipeline::mediate`; цикл здесь — исполнитель той таблицы).
const MAX_ATTEMPTS: u8 = 3;

// Находка 3.12 аудита: инвариант «последняя попытка — Escalate» верен,
// пока попыток ровно MAX_RETRIES+1 — связка зафиксирована compile-time.
const _: () = assert!(
    MAX_ATTEMPTS == berimor_mediation::pipeline::MAX_RETRIES + 1,
    "MAX_ATTEMPTS обязан быть MAX_RETRIES+1 (инвариант unreachable! ниже)"
);

/// Типстёртая ссылка на обобщённый `pipeline::mediate::<C>` конкретного
/// контракта — реестр не может хранить обобщения, хранит мономорфизацию.
type MediateFn = fn(
    step_id: &str,
    raw: &str,
    state: &Value,
    model_tier: Option<ModelTier>,
    rules: &PolicyRules,
    attempt: u8,
    trace: &mut Vec<berimor_types::event::EventKind>,
) -> MediationOutcome<CommitOutcome>;

/// Адаптер контракта: всё, что E2 нужно знать о типе, через который
/// проходит вывод модели. Типстертый доступ к обобщённому `mediate` —
/// по одной fn-ссылке на контракт, реестр открыт для расширения кодом
/// (не конфигурацией: контракты — система типов, M1).
pub struct ContractAdapter {
    pub name: &'static str,
    pub schema_version: u32,
    /// JSON Schema из derive-типа — «подсказка модели собирается из схемы
    /// автоматически» (mediation.md §3), не пишется вручную.
    pub json_schema: fn() -> Value,
    /// Пример из той же версии схемы (executors.md §3).
    pub example: fn() -> Value,
    /// Политики шага: какие поля — ссылки на состояние, какие межполевые
    /// правила действуют. Знает шаг, не контракт вообще (см. pipeline.rs).
    pub policy_rules: fn() -> PolicyRules<'static>,
    /// `pub(crate)`, не `pub`: единственные вызывающие — `execute()` в
    /// этом модуле и `agent_step::AgentStepExecutor::finalize` (E9,
    /// тот же путь валидации финального результата, что у
    /// `LlmStructured`) — оба внутри `berimor-executors`.
    pub(crate) mediate: MediateFn,
}

/// Реестр контрактов Milestone 1: оба контракта golden-процесса. Имена
/// совпадают с полем `contract:` декларации процесса буквально.
pub fn contract_registry() -> &'static [ContractAdapter] {
    static RULES_CLASSIFY: &[policy::CrossFieldRule] =
        &[policy::classification_risk_requires_specific_category];
    static RULES_ANSWER_REFS: &[policy::StateReferenceCheck] = &[policy::StateReferenceCheck {
        output_field: "card_id",
        state_path: "state.user.card_id",
    }];

    &[
        ContractAdapter {
            name: ClassificationOut::NAME,
            schema_version: ClassificationOut::SCHEMA_VERSION,
            json_schema: || {
                serde_json::to_value(schemars::schema_for!(ClassificationOut))
                    .expect("схема derive-типа всегда сериализуема")
            },
            example: || {
                serde_json::json!({
                    "category": "card",
                    "risk": 2,
                    "summary": "Клиент спрашивает о статусе доставки карты."
                })
            },
            policy_rules: || PolicyRules {
                cross_field: RULES_CLASSIFY,
                ..Default::default()
            },
            mediate: |step_id, raw, state, tier, rules, attempt, trace| {
                pipeline::mediate_traced::<ClassificationOut>(
                    step_id, raw, state, tier, rules, attempt, trace,
                )
            },
        },
        ContractAdapter {
            name: SupportReply::NAME,
            schema_version: SupportReply::SCHEMA_VERSION,
            json_schema: || {
                serde_json::to_value(schemars::schema_for!(SupportReply))
                    .expect("схема derive-типа всегда сериализуема")
            },
            example: || {
                serde_json::json!({
                    "card_id": "card_1029",
                    "reply": "Ваша карта активна и будет доставлена в срок."
                })
            },
            policy_rules: || PolicyRules {
                state_references: RULES_ANSWER_REFS,
                ..Default::default()
            },
            mediate: |step_id, raw, state, tier, rules, attempt, trace| {
                pipeline::mediate_traced::<SupportReply>(
                    step_id, raw, state, tier, rules, attempt, trace,
                )
            },
        },
        // Записной путь памяти (memory-model.md §2/§4): извлечение
        // фактов после Finished. Пакетный контракт — пустой пакет
        // легален («запоминать нечего»), до восьми фактов за вызов.
        ContractAdapter {
            name: FactProposalBatch::NAME,
            schema_version: FactProposalBatch::SCHEMA_VERSION,
            json_schema: || {
                serde_json::to_value(schemars::schema_for!(FactProposalBatch))
                    .expect("схема derive-типа всегда сериализуема")
            },
            example: || {
                serde_json::json!({
                    "facts": [{
                        "subject": "card_1029",
                        "predicate": "delivery_status",
                        "object": "in_transit",
                        "confidence": 0.9,
                        "source": "crm.get_card_status"
                    }]
                })
            },
            // Ссылки на состояние НЕ объявлены: факты предлагаются о
            // мире, не привязаны к конкретным полям state; policy-стадия
            // всё равно проверяет утечки по реестру секретов.
            policy_rules: PolicyRules::default,
            mediate: |step_id, raw, state, tier, rules, attempt, trace| {
                pipeline::mediate_traced::<FactProposalBatch>(
                    step_id, raw, state, tier, rules, attempt, trace,
                )
            },
        },
        // Финальный ответ интерактивного режима `berimor chat` (§20.11).
        ContractAdapter {
            name: ChatReply::NAME,
            schema_version: ChatReply::SCHEMA_VERSION,
            json_schema: || {
                serde_json::to_value(schemars::schema_for!(ChatReply))
                    .expect("схема derive-типа всегда сериализуема")
            },
            example: || {
                serde_json::json!({
                    "reply": "Файл output/note.txt создан, в нём 15 байт."
                })
            },
            policy_rules: PolicyRules::default,
            mediate: |step_id, raw, state, tier, rules, attempt, trace| {
                pipeline::mediate_traced::<ChatReply>(
                    step_id, raw, state, tier, rules, attempt, trace,
                )
            },
        },
    ]
}

pub fn find_contract(name: &str) -> Option<&'static ContractAdapter> {
    contract_registry().iter().find(|a| a.name == name)
}

#[derive(Debug, thiserror::Error)]
pub enum StructuredLlmError {
    #[error("неизвестный контракт '{0}' (нет в реестре E2)")]
    UnknownContract(String),
    #[error("нет провайдера, удовлетворяющего требованию шага: {0} — молчаливое понижение класса недопустимо (ideal-agent §3.10)")]
    NoProvider(String),
    #[error("провайдер из реестра не подключён к пулу: '{0}'")]
    ProviderNotWired(String),
    #[error(transparent)]
    Model(#[from] ModelError),
    #[error("эскалация Mediation на стадии {stage:?}: {reason}")]
    Escalated {
        reason: String,
        stage: MediationStage,
    },
    /// TD1.5: `SecurityEvent` уже журналируется хуком `on_attempt` —
    /// здесь только пробрасывается терминальная ошибка шага.
    #[error("инцидент безопасности: {reason}")]
    SecurityViolation { reason: String },
}

/// Исполнитель `llm_structured`-шагов. Собирается один раз на запуск
/// (CLI), не хранит состояния между шагами.
pub struct StructuredLlm<'a> {
    pub pool: &'a ModelPool,
    /// Подключённые провайдеры по имени (`ModelIdentity.provider`).
    pub providers: &'a HashMap<String, &'a dyn berimor_types::executor::ModelProvider>,
    pub context: &'a dyn ContextBuilder,
    /// Телеметрия попыток (M7): запись в журнал — дело вызывающего кода
    /// (`mediation.md` §6); исполнитель сообщает вид события каждой
    /// попытки по таблице `telemetry::outcome_to_event_kind`.
    pub on_attempt: Option<&'a dyn Fn(berimor_types::event::EventKind)>,
    /// Реестр секретов запуска (S5) — наполняет `known_secrets`
    /// policy-стадии контроля утечек (mediation.md §4.3, четвёртая точка
    /// маскировки). Пустой реестр = прежнее поведение (проверка no-op).
    pub secrets: &'a berimor_secrets::Masker,
}

impl StructuredLlm<'_> {
    /// Один шаг: от сборки подсказки до патча. `latency_budget_ms` —
    /// SLA шага из лимитов процесса (ADR-0011).
    pub fn execute(
        &self,
        step_id: &str,
        contract_name: &str,
        tier_requirement: ModelTierRequirement,
        state: &Value,
        latency_budget_ms: Option<u64>,
    ) -> Result<Patch, StructuredLlmError> {
        let adapter = find_contract(contract_name)
            .ok_or_else(|| StructuredLlmError::UnknownContract(contract_name.into()))?;

        // Failover (директива 2026-08-03): недоступность лучшего —
        // следующий кандидат того же класса, не «шаг умер».
        let ranked = self.pool.select_ranked(tier_requirement, latency_budget_ms);
        if ranked.is_empty() {
            return Err(StructuredLlmError::NoProvider(format!(
                "{tier_requirement:?}"
            )));
        }
        let mut candidates = Vec::with_capacity(ranked.len());
        for entry in &ranked {
            let provider = self
                .providers
                .get(&entry.identity.provider)
                .ok_or_else(|| {
                    StructuredLlmError::ProviderNotWired(entry.identity.provider.clone())
                })?;
            candidates.push((entry.identity.provider.as_str(), *provider));
        }
        let model_tier = ranked[0].identity.tier;
        let provider = crate::failover::FailoverProvider::new(candidates, None);

        // task_hint = step_id, не contract_name: только step_id реально
        // журналируется (`EventKind::StepApplied{step_id}`) и потому
        // присутствует в FTS-индексе, по которому ищет слой Session
        // (`memory_builder::session_layer`) — имя контракта в журнал не
        // попадает никогда, поиск по нему был бы декоративным (найдено
        // независимым ревью интеграции CLI-M1/M2/M3).
        let layers = self
            .context
            .build("llm_structured", model_tier, state, step_id);
        let system_context = layers
            .iter()
            .map(|l| format!("## {}\n{}", l.name, l.content))
            .collect::<Vec<_>>()
            .join("\n\n");

        // Контроль утечек (S5): статические правила контракта + значения
        // из реестра запуска. Вектор живёт весь цикл попыток.
        let known_secrets = self.secrets.known_values();

        let mut retry_feedback: Option<String> = None;
        for attempt in 0..MAX_ATTEMPTS {
            let prompt = build_prompt(adapter, step_id, retry_feedback.as_deref());
            let response = provider.complete(CompletionRequest {
                system_context: system_context.clone(),
                prompt,
                contract_name: Some(adapter.name.to_string()),
                expects_structured_output: true,
            })?;

            let mut rules = (adapter.policy_rules)();
            rules.known_secrets = &known_secrets;
            // Трасса стадий (аудит 1.10): parsed/validated идут тем же
            // хуком до события исхода — «каждая стадия пишет событие»
            // (mediation.md §2) теперь достижимо в журнале.
            let mut trace = Vec::new();
            let outcome = (adapter.mediate)(
                step_id,
                &response.raw_text,
                state,
                Some(model_tier),
                &rules,
                attempt,
                &mut trace,
            );

            if let Some(hook) = self.on_attempt {
                for event in trace {
                    hook(event);
                }
                hook(berimor_mediation::telemetry::outcome_to_event_kind(
                    &outcome,
                ));
            }

            match outcome {
                MediationOutcome::Committed(commit) => return Ok(commit.patch),
                // mediation.md §5: «в подсказку добавляется текст ошибки
                // валидации» — повтор собирает новую подсказку, не ту же.
                MediationOutcome::Retry(rejection) => {
                    retry_feedback = Some(format!(
                        "Предыдущий ответ отклонён на стадии {:?}: {}. Исправь и ответь заново.",
                        rejection.stage, rejection.reason
                    ));
                }
                MediationOutcome::Escalate {
                    reason,
                    escalated_from,
                } => {
                    return Err(StructuredLlmError::Escalated {
                        reason,
                        stage: escalated_from,
                    })
                }
                MediationOutcome::SecurityViolation { reason } => {
                    return Err(StructuredLlmError::SecurityViolation { reason })
                }
            }
        }
        unreachable!("последняя попытка завершается Escalate, не Retry (pipeline::mediate)")
    }
}

/// Подсказка — дословно состав из executors.md §3: роль шага + схема
/// контракта + пример из той же версии + (при повторе) текст ошибки.
fn build_prompt(adapter: &ContractAdapter, step_id: &str, retry_feedback: Option<&str>) -> String {
    let mut prompt = format!(
        "Шаг процесса: {step_id}.\n\
         Ответь JSON-объектом по контракту {name} (версия схемы {version}).\n\
         JSON Schema:\n{schema}\n\
         Пример корректного ответа:\n{example}",
        name = adapter.name,
        version = adapter.schema_version,
        schema =
            serde_json::to_string_pretty(&(adapter.json_schema)()).expect("схема сериализуема"),
        example = serde_json::to_string_pretty(&(adapter.example)()).expect("пример сериализуем"),
    );
    if let Some(feedback) = retry_feedback {
        prompt.push_str("\n\n");
        prompt.push_str(feedback);
    }
    prompt
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Пустой реестр — прежнее поведение (контроль утечек no-op).
    static EMPTY_MASKER: berimor_secrets::Masker = berimor_secrets::Masker::new();
    use berimor_context_engine::SimpleContextBuilder;
    use berimor_model_pool::{ModelEntry, ProviderKind};
    use berimor_types::model::{CompletionResponse, ModelIdentity};
    use serde_json::json;
    use std::sync::Mutex;

    /// Провайдер со сценарием ответов и записью запросов — проверяет и
    /// исходы, и то, что повтор несёт текст ошибки в подсказке.
    struct ScriptedProvider {
        responses: Mutex<Vec<String>>,
        requests: Mutex<Vec<CompletionRequest>>,
    }

    impl ScriptedProvider {
        fn new(responses: Vec<&str>) -> Self {
            Self {
                responses: Mutex::new(responses.into_iter().map(String::from).collect()),
                requests: Mutex::new(Vec::new()),
            }
        }
    }

    impl berimor_types::executor::ModelProvider for ScriptedProvider {
        fn complete(&self, request: CompletionRequest) -> Result<CompletionResponse, ModelError> {
            self.requests.lock().unwrap().push(request);
            let mut responses = self.responses.lock().unwrap();
            let raw = if responses.len() > 1 {
                responses.remove(0)
            } else {
                responses[0].clone()
            };
            Ok(CompletionResponse {
                raw_text: raw,
                model: ModelIdentity {
                    provider: "scripted".into(),
                    model_id: "scripted-model".into(),
                    tier: ModelTier::Weak,
                },
            })
        }
    }

    struct Fixture {
        pool: ModelPool,
        providers: HashMap<String, &'static dyn berimor_types::executor::ModelProvider>,
        provider: &'static ScriptedProvider,
    }

    fn fixture(responses: Vec<&str>) -> Fixture {
        // Утечка ссылок допустима в тестах: живут до конца теста.
        let provider: &'static ScriptedProvider =
            Box::leak(Box::new(ScriptedProvider::new(responses)));
        let mut pool = ModelPool::new();
        pool.register(ModelEntry {
            identity: ModelIdentity {
                provider: "scripted".into(),
                model_id: "scripted-model".into(),
                tier: ModelTier::Weak,
            },
            kind: ProviderKind::Local,
            cost_per_1k_tokens: None,
            measured_latency_ms: None,
        });
        let mut providers = HashMap::new();
        providers.insert(
            "scripted".to_string(),
            provider as &'static dyn berimor_types::executor::ModelProvider,
        );
        Fixture {
            pool,
            providers,
            provider,
        }
    }

    #[test]
    fn well_formed_response_commits_on_first_attempt() {
        let f = fixture(vec![
            r#"{"category": "card", "risk": 2, "summary": "Вопрос по карте."}"#,
        ]);
        let executor = StructuredLlm {
            pool: &f.pool,
            providers: &f.providers,
            context: &SimpleContextBuilder,
            on_attempt: None,
            secrets: &EMPTY_MASKER,
        };

        let patch = executor
            .execute(
                "classify",
                "ClassificationOut",
                ModelTierRequirement::Any,
                &json!({"user": {"card_id": "c-1"}}),
                None,
            )
            .unwrap();

        assert_eq!(patch.step_id, "classify");
        assert_eq!(patch.changes["risk"], 2);
        assert_eq!(patch.changes["category"], "card");
    }

    /// `task_hint` обязан быть `step_id`, не `contract_name` — только
    /// `step_id` реально журналируется движком (`StepApplied{step_id}`),
    /// значит только по нему слой Session (`memory_builder`) может
    /// что-то найти в реальном журнале (найдено независимым ревью
    /// интеграции CLI-M1/M2/M3, до этого теста было тихо декоративно).
    struct RecordingContextBuilder {
        seen_task_hint: Mutex<Option<String>>,
    }

    impl berimor_context_engine::ContextBuilder for RecordingContextBuilder {
        fn build(
            &self,
            _step_kind: &str,
            _tier: ModelTier,
            _state: &Value,
            task_hint: &str,
        ) -> Vec<berimor_context_engine::ContextLayer> {
            *self.seen_task_hint.lock().unwrap() = Some(task_hint.to_string());
            Vec::new()
        }
    }

    #[test]
    fn context_builder_receives_step_id_as_task_hint_not_contract_name() {
        let f = fixture(vec![
            r#"{"category": "card", "risk": 2, "summary": "Вопрос по карте."}"#,
        ]);
        let context = RecordingContextBuilder {
            seen_task_hint: Mutex::new(None),
        };
        let executor = StructuredLlm {
            pool: &f.pool,
            providers: &f.providers,
            context: &context,
            on_attempt: None,
            secrets: &EMPTY_MASKER,
        };

        executor
            .execute(
                "classify",
                "ClassificationOut",
                ModelTierRequirement::Any,
                &json!({}),
                None,
            )
            .unwrap();

        assert_eq!(
            context.seen_task_hint.lock().unwrap().as_deref(),
            Some("classify"),
            "task_hint обязан быть step_id ('classify'), не именем контракта"
        );
    }

    #[test]
    fn invalid_first_response_retries_with_error_text_in_prompt() {
        let f = fixture(vec![
            "не json вообще",
            r#"{"category": "debt", "risk": 5, "summary": "После повтора."}"#,
        ]);
        let executor = StructuredLlm {
            pool: &f.pool,
            providers: &f.providers,
            context: &SimpleContextBuilder,
            on_attempt: None,
            secrets: &EMPTY_MASKER,
        };

        let patch = executor
            .execute(
                "classify",
                "ClassificationOut",
                ModelTierRequirement::Any,
                &json!({}),
                None,
            )
            .unwrap();

        assert_eq!(patch.changes["summary"], "После повтора.");
        let requests = f.provider.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert!(
            !requests[0].prompt.contains("отклонён"),
            "первая подсказка не должна нести текст ошибки"
        );
        assert!(
            requests[1].prompt.contains("отклонён"),
            "повтор обязан нести текст ошибки валидации (mediation.md §5): {}",
            requests[1].prompt
        );
    }

    #[test]
    fn persistent_failure_escalates_after_all_attempts() {
        let f = fixture(vec!["мусор"]);
        let executor = StructuredLlm {
            pool: &f.pool,
            providers: &f.providers,
            context: &SimpleContextBuilder,
            on_attempt: None,
            secrets: &EMPTY_MASKER,
        };

        let result = executor.execute(
            "classify",
            "ClassificationOut",
            ModelTierRequirement::Any,
            &json!({}),
            None,
        );

        assert!(matches!(result, Err(StructuredLlmError::Escalated { .. })));
        assert_eq!(
            f.provider.requests.lock().unwrap().len() as u8,
            MAX_ATTEMPTS,
            "попыток ровно MAX_ATTEMPTS, не больше"
        );
    }

    #[test]
    fn policy_violation_escalates_without_retry() {
        // risk >= 7 с category 'other' — межполевое правило из golden-
        // контракта; policy не лечится повтором (mediation.md §5).
        let f = fixture(vec![
            r#"{"category": "other", "risk": 9, "summary": "Опасный случай."}"#,
        ]);
        let executor = StructuredLlm {
            pool: &f.pool,
            providers: &f.providers,
            context: &SimpleContextBuilder,
            on_attempt: None,
            secrets: &EMPTY_MASKER,
        };

        let result = executor.execute(
            "classify",
            "ClassificationOut",
            ModelTierRequirement::Any,
            &json!({}),
            None,
        );

        match result {
            Err(StructuredLlmError::Escalated { stage, .. }) => {
                assert_eq!(stage, MediationStage::Policy)
            }
            other => panic!("ожидалась эскалация policy: {other:?}"),
        }
        assert_eq!(f.provider.requests.lock().unwrap().len(), 1);
    }

    #[test]
    fn unknown_contract_is_an_error_not_a_guess() {
        let f = fixture(vec!["{}"]);
        let executor = StructuredLlm {
            pool: &f.pool,
            providers: &f.providers,
            context: &SimpleContextBuilder,
            on_attempt: None,
            secrets: &EMPTY_MASKER,
        };
        let result = executor.execute(
            "x",
            "NoSuchContract",
            ModelTierRequirement::Any,
            &json!({}),
            None,
        );
        assert!(matches!(
            result,
            Err(StructuredLlmError::UnknownContract(_))
        ));
    }

    #[test]
    fn missing_provider_tier_is_an_error_not_silent_downgrade() {
        let f = fixture(vec!["{}"]);
        let executor = StructuredLlm {
            pool: &f.pool,
            providers: &f.providers,
            context: &SimpleContextBuilder,
            on_attempt: None,
            secrets: &EMPTY_MASKER,
        };
        let result = executor.execute(
            "classify",
            "ClassificationOut",
            ModelTierRequirement::Strong,
            &json!({}),
            None,
        );
        assert!(matches!(result, Err(StructuredLlmError::NoProvider(_))));
    }

    /// Композиция на golden-фикстуре процесса: шаг `answer` с контрактом
    /// SupportReply проходит policy-ссылку на состояние (card_id обязан
    /// совпасть с state.user.card_id), шаг `classify` — как в процессе.
    #[test]
    fn composes_with_golden_process_steps() {
        const GOLDEN: &str =
            include_str!("../../../fixtures/golden/processes/card-delivery-support.yaml");
        let process = berimor_process_engine::parser::parse(GOLDEN).unwrap();

        let f = fixture(vec![
            r#"{"category": "card", "risk": 2, "summary": "Доставка карты."}"#,
            r#"{"card_id": "card_1029", "reply": "Карта активна."}"#,
        ]);
        let executor = StructuredLlm {
            pool: &f.pool,
            providers: &f.providers,
            context: &SimpleContextBuilder,
            on_attempt: None,
            secrets: &EMPTY_MASKER,
        };
        let state = json!({"user": {"card_id": "card_1029"}});

        for step_id in ["classify", "answer"] {
            let step = process.steps.iter().find(|s| s.id == step_id).unwrap();
            let berimor_types::step::StepKind::LlmStructured {
                contract,
                model_tier,
            } = &step.kind
            else {
                panic!("ожидался LlmStructured");
            };
            let patch = executor
                .execute(&step.id, contract, *model_tier, &state, None)
                .unwrap();
            assert_eq!(patch.step_id, step_id);
        }
    }

    /// S5, точка 4: с наполненным реестром вывод модели, содержащий
    /// значение секрета, — инцидент безопасности (SecurityViolation,
    /// mediation.md §5: «падение процесса + событие безопасности»), а не
    /// обычный отказ. С пустым реестром тот же вывод прошёл бы — это и
    /// был мёртвый код до S5.
    #[test]
    fn populated_registry_turns_secret_leak_into_security_violation() {
        const SECRET: &str = "sk-test-FAKESECRET-9f8e7d6c";
        let f = fixture(vec![&format!(
            r#"{{"card_id": "card_1029", "reply": "ваш ключ {SECRET}"}}"#
        )]);
        let mut masker = berimor_secrets::Masker::new();
        masker.register(berimor_secrets::Secret::new(SECRET.into()));
        let executor = StructuredLlm {
            pool: &f.pool,
            providers: &f.providers,
            context: &SimpleContextBuilder,
            on_attempt: None,
            secrets: &masker,
        };

        let result = executor.execute(
            "answer",
            "SupportReply",
            ModelTierRequirement::Any,
            &json!({"user": {"card_id": "card_1029"}}),
            None,
        );

        assert!(
            matches!(result, Err(StructuredLlmError::SecurityViolation { .. })),
            "утечка секрета обязана быть инцидентом безопасности: {result:?}"
        );
        // SecurityViolation — без повтора (mediation.md §5: 0 повторов).
        assert_eq!(f.provider.requests.lock().unwrap().len(), 1);
    }

    #[test]
    fn answer_step_rejects_forged_card_id_via_policy() {
        let f = fixture(vec![r#"{"card_id": "card_9999_forged", "reply": "..."}"#]);
        let executor = StructuredLlm {
            pool: &f.pool,
            providers: &f.providers,
            context: &SimpleContextBuilder,
            on_attempt: None,
            secrets: &EMPTY_MASKER,
        };

        let result = executor.execute(
            "answer",
            "SupportReply",
            ModelTierRequirement::Any,
            &json!({"user": {"card_id": "card_1029"}}),
            None,
        );

        match result {
            Err(StructuredLlmError::Escalated { stage, .. }) => {
                assert_eq!(stage, MediationStage::Policy)
            }
            other => panic!("подделка card_id обязана эскалировать: {other:?}"),
        }
    }
}
