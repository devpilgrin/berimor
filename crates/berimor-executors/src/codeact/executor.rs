//! `CodeActExecutor` — исполнитель `codeact`-шагов (ROADMAP E8, финальный
//! кусок): модель пишет программу на каждом вызове шага
//! (`executors.md` §4.1), как `StructuredLlm`/`AgentStep` пишут
//! JSON-ответ/ход. Цикл на попытку (`MAX_ATTEMPTS`, тот же паттерн, что
//! `StructuredLlm`): подсказка → модель → JS-текст → статический анализ
//! ([`super::static_analysis::analyze`], E7) → песочница
//! ([`super::wasm_host::WasmHost::run`], E6/E8) → Mediation результата
//! против контракта шага (`structured_llm::find_contract`, тот же путь,
//! что `LlmStructured`/`AgentStep::finalize`) → commit | повтор новым
//! промптом | эскалация.
//!
//! В отличие от `AgentStep` (E9), где ретрай — это ещё один ХОД внутри
//! ОДНОГО цикла рассуждения, здесь ретрай — это НОВАЯ ПРОГРАММА
//! ЦЕЛИКОМ: отказ статического анализа, сбой песочницы (трап,
//! исчерпание лимита, невалидный вход/выход гостя) и отказ Mediation —
//! все три одинаково становятся текстом обратной связи для СЛЕДУЮЩЕЙ
//! попытки модели написать программу заново, не для продолжения той же
//! программы (единственный выход программы — `finish`, редактировать
//! уже завершившийся прогон нечем).
//!
//! `tools: &[String]` — явный список имён стабов, доступных ИМЕННО
//! этой программе (`StepKind::CodeAct.tools`) — используется и белым
//! списком статического анализа, и подсказкой модели. В отличие от
//! `AgentStep` (осознанный пробел E9 — там доступен весь
//! `CompositeToolDispatch` конфига), здесь сужение есть с самого
//! начала: `WasmHost`, который реально диспетчит вызовы, по-прежнему
//! видит весь `ToolDispatch`, переданный при конструировании — `tools` сужает
//! только то, что МОДЕЛЬ ЗНАЕТ И ЧТО ПРОЙДЁТ статический анализ, не
//! сам диспетч (тот факт, что программа технически МОГЛА бы сослаться
//! на инструмент вне списка, если бы обошла статический анализ,
//! закрывается capability-гейтом на каждый вызов ровно так же, как у
//! `ToolOnly`/`AgentStep` — двойная, не единственная, линия обороны).
//!
//! `WasmLimits` выбирается по фактическому классу отобранного
//! провайдера (`WasmLimits::strong()`/`reduced()`, `super::wasm_host`) —
//! частичная реализация допуска по классам моделей (`executors.md`
//! §4.3): «слабый — только с явным разрешением в процессе» НЕ
//! проверяется (в системе нет понятия такого разрешения ни для одного
//! шага) — тот же класс честно не закрытого пробела, что
//! `capability_ceiling` у `AgentStep`.

use crate::codeact::static_analysis;
use crate::codeact::wasm_host::{self, WasmHost, WasmLimits};
use crate::structured_llm;
use berimor_context_engine::ContextBuilder;
use berimor_model_pool::ModelPool;
use berimor_types::{
    executor::ModelProvider,
    mediation::{MediationOutcome, MediationStage},
    model::{CompletionRequest, ModelError, ModelTier, ModelTierRequirement},
    step::Patch,
};
use serde_json::Value;
use std::collections::HashMap;

/// До 3 попыток написать программу, проходящую все три барьера
/// (статический анализ → песочница → Mediation) — тот же паттерн и
/// число, что у `StructuredLlm`/`AgentStep::decide_turn`.
const MAX_ATTEMPTS: u8 = 3;

#[derive(Debug, thiserror::Error)]
pub enum CodeActError {
    #[error("неизвестный контракт '{0}' (нет в реестре E2) для результата CodeAct")]
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
    #[error("исчерпаны попытки ({max_attempts}) написать программу, проходящую все проверки")]
    AttemptsExhausted { max_attempts: u8 },
    /// TD1.5: `SecurityEvent` уже журналируется хуком `on_attempt`.
    #[error("инцидент безопасности: {reason}")]
    SecurityViolation { reason: String },
}

/// Исполнитель `codeact`-шагов. `wasm_host` уже несёт `dispatch`/`gate`/
/// `mode`/`confirmer` (собран один раз при старте CLI, как и у
/// `ToolOnly`/`AgentStep`) — этому исполнителю нужна только ссылка на
/// него, не отдельные поля.
pub struct CodeActExecutor<'a> {
    pub pool: &'a ModelPool,
    pub providers: &'a HashMap<String, &'a dyn ModelProvider>,
    pub context: &'a dyn ContextBuilder,
    pub on_attempt: Option<&'a dyn Fn(berimor_types::event::EventKind)>,
    pub wasm_host: &'a WasmHost,
    /// Реестр секретов запуска (S5) — контроль утечек policy-стадии над
    /// РЕЗУЛЬТАТОМ программы (mediation.md §4.3): `finish(result)` может
    /// протащить значение из состояния мимо замаскированных наблюдений
    /// `call_tool` (находка 2 независимого ревью S5).
    pub secrets: &'a berimor_secrets::Masker,
}

impl CodeActExecutor<'_> {
    /// `tools` — `StepKind::CodeAct.tools`, `contract_name` —
    /// `StepKind::CodeAct.contract`, `tier_requirement` —
    /// `StepKind::CodeAct.model_tier`. `latency_budget_ms` — тот же SLA
    /// отбора провайдера, что у `StructuredLlm`/`AgentStep` (ADR-0011).
    pub fn execute(
        &self,
        step_id: &str,
        contract_name: &str,
        tools: &[String],
        tier_requirement: ModelTierRequirement,
        state: &Value,
        latency_budget_ms: Option<u64>,
    ) -> Result<Patch, CodeActError> {
        let adapter = structured_llm::find_contract(contract_name)
            .ok_or_else(|| CodeActError::UnknownContract(contract_name.into()))?;

        let entry = self
            .pool
            .select(tier_requirement, latency_budget_ms)
            .ok_or_else(|| CodeActError::NoProvider(format!("{tier_requirement:?}")))?;
        let provider = *self
            .providers
            .get(&entry.identity.provider)
            .ok_or_else(|| CodeActError::ProviderNotWired(entry.identity.provider.clone()))?;
        let model_tier = entry.identity.tier;

        // task_hint = step_id, не contract_name — только step_id реально
        // журналируется (тот же выбор, что `StructuredLlm`, найдено
        // независимым ревью интеграции CLI-M1/M2/M3).
        let layers = self.context.build("codeact", model_tier, state, step_id);
        let system_context = layers
            .iter()
            .map(|l| format!("## {}\n{}", l.name, l.content))
            .collect::<Vec<_>>()
            .join("\n\n");

        let limits = match model_tier {
            ModelTier::Strong => WasmLimits::strong(),
            ModelTier::Medium | ModelTier::Weak => WasmLimits::reduced(),
        };

        let mut retry_feedback: Option<String> = None;

        for attempt in 0..MAX_ATTEMPTS {
            let prompt = build_prompt(step_id, adapter, tools, retry_feedback.as_deref());
            let response = provider.complete(CompletionRequest {
                system_context: system_context.clone(),
                prompt,
                contract_name: Some(adapter.name.to_string()),
                // TD3.3: ответ модели — текст JS-программы, не JSON по
                // контракту (контракт применяется позже, к результату
                // исполнения программы, не к самому ответу).
                expects_structured_output: false,
            })?;

            if let Err(violation) = static_analysis::analyze(
                &response.raw_text,
                &tools.iter().map(String::as_str).collect::<Vec<_>>(),
            ) {
                retry_feedback = Some(format!(
                    "Статический анализ отклонил программу: {violation}. \
                     Перепиши, используя только разрешённые идентификаторы."
                ));
                continue;
            }

            let program_input = serde_json::json!({
                "program": response.raw_text,
                "input": state,
            });
            let result_value =
                match self
                    .wasm_host
                    .run(wasm_host::GUEST_WASM, &program_input, &limits, tools)
                {
                    Ok(value) => value,
                    Err(err) => {
                        retry_feedback = Some(format!(
                            "Программа завершилась с ошибкой при исполнении: {err}. \
                             Проверь программу и попробуй снова."
                        ));
                        continue;
                    }
                };

            let raw = serde_json::to_string(&result_value)
                .expect("Value всегда сериализуем в JSON-текст");
            // Контроль утечек (S5): статические правила контракта +
            // значения из реестра запуска — как у StructuredLlm/AgentStep.
            let known_secrets = self.secrets.known_values();
            let mut rules = (adapter.policy_rules)();
            rules.known_secrets = &known_secrets;
            let outcome =
                (adapter.mediate)(step_id, &raw, state, Some(model_tier), &rules, attempt);

            if let Some(hook) = self.on_attempt {
                hook(berimor_mediation::telemetry::outcome_to_event_kind(
                    &outcome,
                ));
            }

            match outcome {
                MediationOutcome::Committed(commit) => return Ok(commit.patch),
                MediationOutcome::Retry(rejection) => {
                    retry_feedback = Some(format!(
                        "Результат программы отклонён на стадии {:?}: {}. Перепиши программу.",
                        rejection.stage, rejection.reason
                    ));
                }
                MediationOutcome::Escalate {
                    reason,
                    escalated_from,
                } => {
                    return Err(CodeActError::Escalated {
                        reason,
                        stage: escalated_from,
                    })
                }
                MediationOutcome::SecurityViolation { reason } => {
                    return Err(CodeActError::SecurityViolation { reason })
                }
            }
        }

        Err(CodeActError::AttemptsExhausted {
            max_attempts: MAX_ATTEMPTS,
        })
    }
}

/// Подсказка попытки: схема + пример контракта результата, доступные
/// имена инструментов, белый список статического анализа (чтобы модель
/// не тратила попытки на заведомо запрещённые идентификаторы), текст
/// обратной связи от прошлой попытки, если есть.
fn build_prompt(
    step_id: &str,
    adapter: &structured_llm::ContractAdapter,
    tools: &[String],
    retry_feedback: Option<&str>,
) -> String {
    let schema = serde_json::to_string_pretty(&(adapter.json_schema)())
        .expect("схема контракта всегда сериализуема");
    let example = serde_json::to_string_pretty(&(adapter.example)())
        .expect("пример контракта всегда сериализуем");
    let tools_list = if tools.is_empty() {
        "(нет доступных инструментов для этой программы)".to_string()
    } else {
        tools.join(", ")
    };
    let safe_globals = static_analysis::SAFE_GLOBALS.join(", ");

    let mut prompt = format!(
        "Шаг процесса: {step_id}. Напиши ПРОГРАММУ на JavaScript (executors.md §4.1) — не JSON-ответ.\n\n\
         Единственный выход программы — вызов `finish(result)`; `result` ОБЯЗАН \
         соответствовать контракту {name} (версия {version}):\n{schema}\n\
         Пример корректного result: {example}\n\n\
         Доступные функции внутри программы:\n\
         - `finish(result)` — завершает программу, `result` становится итогом шага.\n\
         - `call_tool(name, args) -> {{ok, value|error}}` — вызов стаба инструмента; НЕ бросает \
         исключение при отказе, проверяй `.ok` сам. Доступные для этой программы имена \
         инструментов: {tools_list}.\n\
         - Глобальная переменная `input` — срез состояния процесса.\n\n\
         До исполнения программа проходит статический анализ (белый список идентификаторов, \
         executors.md §4.2): разрешены только {safe_globals} и перечисленные выше имена \
         инструментов — любой другой свободный идентификатор (включая eval/Function/fetch и \
         подобные) отклоняется ДО исполнения, попытка использовать их — трата попытки впустую.",
        name = adapter.name,
        version = adapter.schema_version,
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
    use crate::tool_only::{ConfirmationHandler, DispatchError, ToolDispatch};
    use berimor_capability::CapabilityGate;
    use berimor_context_engine::SimpleContextBuilder;
    use berimor_model_pool::{ModelEntry, ProviderKind};
    use berimor_types::capability::{CapabilityDecision, ConfirmationMode, ProposedAction};
    use berimor_types::model::{CompletionResponse, ModelIdentity};
    use serde_json::json;
    use std::sync::{Arc, Mutex};

    struct ScriptedProvider {
        responses: Mutex<Vec<String>>,
    }

    impl ScriptedProvider {
        fn new(responses: Vec<&str>) -> Self {
            Self {
                responses: Mutex::new(responses.into_iter().map(String::from).collect()),
            }
        }
    }

    impl ModelProvider for ScriptedProvider {
        fn complete(&self, _request: CompletionRequest) -> Result<CompletionResponse, ModelError> {
            let mut responses = self.responses.lock().unwrap();
            assert!(
                !responses.is_empty(),
                "сценарий исчерпан раньше, чем ожидалось"
            );
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

    struct AllowAll;
    impl CapabilityGate for AllowAll {
        fn check(&self, _action: &ProposedAction, _mode: ConfirmationMode) -> CapabilityDecision {
            CapabilityDecision::Allow
        }
    }

    struct DenyAll;
    impl CapabilityGate for DenyAll {
        fn check(&self, _action: &ProposedAction, _mode: ConfirmationMode) -> CapabilityDecision {
            CapabilityDecision::Deny {
                reason: "заблокировано тестом".to_string(),
            }
        }
    }

    struct AutoConfirm;
    impl ConfirmationHandler for AutoConfirm {
        fn confirm(&self, _action: &ProposedAction, _reason: &str) -> bool {
            true
        }
    }

    struct NoopDispatch;
    impl ToolDispatch for NoopDispatch {
        fn call(&self, tool: &str, _args: &Value) -> Result<Value, DispatchError> {
            Err(DispatchError {
                tool: tool.to_string(),
                reason: "в этих тестах программы не вызывают инструменты".to_string(),
            })
        }
    }

    /// Диспетч, который обязан НИКОГДА не быть вызван — если тест
    /// проходит, а этот двойник получил вызов, capability-гейт был
    /// обойдён где-то на пути `CodeActExecutor` → `WasmHost` →
    /// `host_call_tool` → `tool_only::dispatch_confirmed` (тот же
    /// приём, что `wasm_host.rs`/`agent_step.rs` — независимое ревью
    /// E8 отметило, что на уровне САМОГО `CodeActExecutor` такого
    /// теста не было, только внутри `WasmHost` в изоляции).
    struct PanicIfCalledDispatch;
    impl ToolDispatch for PanicIfCalledDispatch {
        fn call(&self, _tool: &str, _args: &Value) -> Result<Value, DispatchError> {
            panic!("capability-гейт обойдён: диспетч вызван после отказа");
        }
    }

    fn test_wasm_host() -> WasmHost {
        WasmHost::new(
            Arc::new(NoopDispatch),
            Arc::new(AllowAll),
            ConfirmationMode::Smart,
            Arc::new(AutoConfirm),
            Arc::new(berimor_secrets::Masker::new()),
        )
    }

    fn test_wasm_host_deny_all() -> WasmHost {
        WasmHost::new(
            Arc::new(PanicIfCalledDispatch),
            Arc::new(DenyAll),
            ConfirmationMode::Smart,
            Arc::new(AutoConfirm),
            Arc::new(berimor_secrets::Masker::new()),
        )
    }

    fn pool_and_providers(
        provider: &'static ScriptedProvider,
    ) -> (ModelPool, HashMap<String, &'static dyn ModelProvider>) {
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
        let mut providers: HashMap<String, &'static dyn ModelProvider> = HashMap::new();
        providers.insert("scripted".into(), provider);
        (pool, providers)
    }

    const VALID_PROGRAM: &str = "finish({card_id: input.user.card_id, reply: 'готово'})";
    const FORGED_PROGRAM: &str = "finish({card_id: 'forged', reply: 'готово'})";
    const FORBIDDEN_IDENTIFIER_PROGRAM: &str = "eval('1')";
    const THROWING_PROGRAM: &str = "throw new Error('boom')";
    /// Зовёт стаб инструмента и кладёт ответ на отказ прямо в `reply` —
    /// позволяет тесту убедиться, что отказ capability-гейта реально
    /// долетел до ЗАПУЩЕННОЙ программы (не только до `WasmHost` в
    /// изоляции), не переписывая отдельный протокол проверки для этого
    /// одного теста.
    const CALL_TOOL_THEN_FINISH_PROGRAM: &str = "\
        var r = call_tool('some_tool', {});\
        finish({card_id: input.user.card_id, reply: r.ok ? 'unexpected-success' : r.error});";

    #[test]
    fn happy_path_produces_a_patch() {
        let provider: &'static ScriptedProvider =
            Box::leak(Box::new(ScriptedProvider::new(vec![VALID_PROGRAM])));
        let (pool, providers) = pool_and_providers(provider);
        let host = test_wasm_host();
        let executor = CodeActExecutor {
            pool: &pool,
            providers: &providers,
            context: &SimpleContextBuilder,
            on_attempt: None,
            wasm_host: &host,
            secrets: &EMPTY_MASKER,
        };

        let state = json!({"user": {"card_id": "c-1"}});
        let patch = executor
            .execute(
                "answer",
                "SupportReply",
                &[],
                ModelTierRequirement::Any,
                &state,
                None,
            )
            .unwrap();

        assert_eq!(patch.step_id, "answer");
        assert_eq!(patch.changes["card_id"], "c-1");
        assert_eq!(patch.changes["reply"], "готово");
    }

    #[test]
    fn static_analysis_rejection_triggers_retry_with_a_fresh_program() {
        let provider: &'static ScriptedProvider = Box::leak(Box::new(ScriptedProvider::new(vec![
            FORBIDDEN_IDENTIFIER_PROGRAM,
            VALID_PROGRAM,
        ])));
        let (pool, providers) = pool_and_providers(provider);
        let host = test_wasm_host();
        let executor = CodeActExecutor {
            pool: &pool,
            providers: &providers,
            context: &SimpleContextBuilder,
            on_attempt: None,
            wasm_host: &host,
            secrets: &EMPTY_MASKER,
        };

        let state = json!({"user": {"card_id": "c-1"}});
        let patch = executor
            .execute(
                "answer",
                "SupportReply",
                &[],
                ModelTierRequirement::Any,
                &state,
                None,
            )
            .unwrap();

        assert_eq!(patch.changes["reply"], "готово");
    }

    #[test]
    fn sandbox_failure_triggers_retry_with_a_fresh_program() {
        let provider: &'static ScriptedProvider = Box::leak(Box::new(ScriptedProvider::new(vec![
            THROWING_PROGRAM,
            VALID_PROGRAM,
        ])));
        let (pool, providers) = pool_and_providers(provider);
        let host = test_wasm_host();
        let executor = CodeActExecutor {
            pool: &pool,
            providers: &providers,
            context: &SimpleContextBuilder,
            on_attempt: None,
            wasm_host: &host,
            secrets: &EMPTY_MASKER,
        };

        let state = json!({"user": {"card_id": "c-1"}});
        let patch = executor
            .execute(
                "answer",
                "SupportReply",
                &[],
                ModelTierRequirement::Any,
                &state,
                None,
            )
            .unwrap();

        assert_eq!(patch.changes["reply"], "готово");
    }

    /// Тот же путь Mediation, что у `StructuredLlm`/`AgentStep`
    /// (`state-reference-forgery.json`): программа, вернувшая
    /// сфабрикованный `card_id`, не совпадающий с `state.user.card_id`,
    /// отклоняется на стадии Policy, а не молча записывается в
    /// состояние.
    #[test]
    fn forged_state_reference_is_rejected_by_policy() {
        let provider: &'static ScriptedProvider =
            Box::leak(Box::new(ScriptedProvider::new(vec![FORGED_PROGRAM])));
        let (pool, providers) = pool_and_providers(provider);
        let host = test_wasm_host();
        let executor = CodeActExecutor {
            pool: &pool,
            providers: &providers,
            context: &SimpleContextBuilder,
            on_attempt: None,
            wasm_host: &host,
            secrets: &EMPTY_MASKER,
        };

        let state = json!({"user": {"card_id": "c-1"}});
        let result = executor.execute(
            "answer",
            "SupportReply",
            &[],
            ModelTierRequirement::Any,
            &state,
            None,
        );

        match result {
            Err(CodeActError::Escalated { stage, .. }) => {
                assert_eq!(stage, MediationStage::Policy)
            }
            other => panic!("ожидалась эскалация на стадии Policy: {other:?}"),
        }
    }

    #[test]
    fn attempts_exhausted_is_an_error_not_a_silent_empty_patch() {
        let provider: &'static ScriptedProvider = Box::leak(Box::new(ScriptedProvider::new(vec![
            FORBIDDEN_IDENTIFIER_PROGRAM,
        ])));
        let (pool, providers) = pool_and_providers(provider);
        let host = test_wasm_host();
        let executor = CodeActExecutor {
            pool: &pool,
            providers: &providers,
            context: &SimpleContextBuilder,
            on_attempt: None,
            wasm_host: &host,
            secrets: &EMPTY_MASKER,
        };

        let state = json!({"user": {"card_id": "c-1"}});
        let result = executor.execute(
            "answer",
            "SupportReply",
            &[],
            ModelTierRequirement::Any,
            &state,
            None,
        );

        assert!(matches!(
            result,
            Err(CodeActError::AttemptsExhausted { max_attempts: 3 })
        ));
    }

    /// Найдено независимым ревью E8: `capability_deny_blocks_call_tool_
    /// before_dispatch_is_ever_called` в `wasm_host.rs` доказывает это
    /// для `WasmHost` в изоляции, но НЕ для полной цепочки, которую
    /// реально собирает `CliExecutor` (`CodeActExecutor` →
    /// `WasmHost`) — этот тест закрывает именно её: отказ гейта обязан
    /// долететь до ЗАПУЩЕННОЙ программы через `call_tool(...)`, не
    /// обходя `dispatch_confirmed` (`PanicIfCalledDispatch` паникует,
    /// если диспетч всё же вызван).
    #[test]
    fn capability_deny_reaches_the_running_program_not_bypassed() {
        let provider: &'static ScriptedProvider = Box::leak(Box::new(ScriptedProvider::new(vec![
            CALL_TOOL_THEN_FINISH_PROGRAM,
        ])));
        let (pool, providers) = pool_and_providers(provider);
        let host = test_wasm_host_deny_all();
        let executor = CodeActExecutor {
            pool: &pool,
            providers: &providers,
            context: &SimpleContextBuilder,
            on_attempt: None,
            wasm_host: &host,
            secrets: &EMPTY_MASKER,
        };

        let state = json!({"user": {"card_id": "c-1"}});
        let patch = executor
            .execute(
                "answer",
                "SupportReply",
                &["some_tool".to_string()],
                ModelTierRequirement::Any,
                &state,
                None,
            )
            .unwrap();

        assert_eq!(patch.changes["card_id"], "c-1");
        assert!(patch.changes["reply"]
            .as_str()
            .unwrap()
            .contains("заблокировано тестом"));
    }

    #[test]
    fn unknown_contract_is_an_error_before_any_model_call() {
        let provider: &'static ScriptedProvider =
            Box::leak(Box::new(ScriptedProvider::new(vec![VALID_PROGRAM])));
        let (pool, providers) = pool_and_providers(provider);
        let host = test_wasm_host();
        let executor = CodeActExecutor {
            pool: &pool,
            providers: &providers,
            context: &SimpleContextBuilder,
            on_attempt: None,
            wasm_host: &host,
            secrets: &EMPTY_MASKER,
        };

        let result = executor.execute(
            "answer",
            "NoSuchContract",
            &[],
            ModelTierRequirement::Any,
            &json!({}),
            None,
        );

        assert!(matches!(result, Err(CodeActError::UnknownContract(_))));
    }
}
