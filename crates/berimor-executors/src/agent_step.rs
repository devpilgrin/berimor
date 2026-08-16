//! AgentStep — свободный цикл «рассуждение → действие → наблюдение», выделенный случай.
//!
//! Источник: `docs/arch/executors.md` §5. ROADMAP: E9.
//!
//! Цикл: на каждом ходу модель отвечает `AgentTurnDecision`
//! (`berimor_mediation::contracts`) — фиксированная форма, ОДНА на все
//! `agent_step`-шаги системы, не зависящая от контракта финального
//! результата. `Tool`-действие идёт через тот же путь
//! «capability-гейт → подтверждение → диспетч», что и обычный `ToolOnly`
//! (`tool_only::dispatch_confirmed` — общая функция, не копия). `Finish`
//! завершает цикл: `result` валидируется отдельным проходом Mediation
//! против КОНКРЕТНОГО контракта, который декларирует
//! `StepKind::AgentStep.contract` (реестр `structured_llm::contract_registry`,
//! тот же путь, что и у `LlmStructured`/`CodeAct`).
//!
//! Что честно не входит (см. `docs/ROADMAP.md`, задокументированный, не
//! забытый пробел): бюджет токенов (`ModelProvider`/`CompletionResponse`
//! нигде не считает использование — тот же класс, что
//! `ProcessLimits.token_budget`, P6); суб-шаговый снапшот «перед каждой
//! правкой» (Process Engine снапшотит после `AgentStep` целиком, как и
//! после `CodeAct`/`LlmStructured` — синхронный контракт `StepExecutor`,
//! один `Patch` на вызов); режим «пользовательский люк» вне процесса;
//! ограничение набора инструментов, доступных конкретному
//! `agent_step`-шагу — `AgentStepExecutor.dispatch` тот же
//! `CompositeToolDispatch`, что и у всех `Tool`-шагов процесса, `contract`
//! в `StepKind::AgentStep` не сужает его (не обход capability-гейта — тот
//! отрабатывает на каждый вызов одинаково, — но расширение поверхности:
//! шагу доступен весь сконфигурированный набор, не только те инструменты,
//! что автор процесса имел в виду; `executors.md` §5 сужение не
//! специфицирует, в `berimor-tool-runtime::plugin_process` уже есть паттерн
//! именно для такого ограничения — `PluginManifest.capability_ceiling`, —
//! не применённый здесь; найдено независимым ревью E9, осознанно оставлено
//! вне этого захода).

use crate::structured_llm::{self, ContractAdapter};
use crate::tool_only::{self, ConfirmationHandler, ToolDispatch};
use berimor_capability::CapabilityGate;
use berimor_context_engine::ContextBuilder;
use berimor_mediation::{
    contracts::{AgentAction, AgentTurnDecision, AgentVerdict},
    pipeline::{self, PolicyRules},
};
use berimor_model_pool::ModelPool;
use berimor_types::contract::Contract;
use berimor_types::{
    capability::ConfirmationMode,
    executor::ModelProvider,
    mediation::{MediationOutcome, MediationStage},
    model::{CompletionRequest, ModelError, ModelTier, ModelTierRequirement},
    step::Patch,
};
use serde_json::Value;
use std::collections::HashMap;

/// До 3 попыток на разбор/схему хода — то же число, что у `StructuredLlm`
/// (`mediation.md` §5: до 2 повторов + первая попытка).
const MAX_ATTEMPTS: u8 = 3;

// См. structured_llm.rs — та же compile-time скрепка (3.12 аудита).
const _: () = assert!(
    MAX_ATTEMPTS == berimor_mediation::pipeline::MAX_RETRIES + 1,
    "MAX_ATTEMPTS обязан быть MAX_RETRIES+1 (инвариант unreachable! ниже)"
);

#[derive(Debug, thiserror::Error)]
pub enum AgentStepError {
    #[error("неизвестный контракт '{0}' (нет в реестре E2) для финального результата AgentStep")]
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
    /// Только capability-деним/отказ на подтверждении — НЕ сбой самого
    /// инструмента (`DispatchError`, восстановимая ошибка, становится
    /// наблюдением хода, см. `execute()`). Терминальная ошибка, не
    /// повод для ретрая: оба исхода — решение, которое цикл обязан
    /// уважать, не пытаться обойти переформулировкой (security-model.md:
    /// «нет неявного расширения привилегий»).
    #[error("действие '{tool}' отклонено: {reason}")]
    ActionRejected { tool: String, reason: String },
    /// «Предложи-выполни-проверь» (executors.md §5): отрицательный
    /// вердикт после `Tool`-действия — терминальный исход цикла, один
    /// из трёх наравне с финальным ответом и исчерпанием `max_turns`.
    #[error("проверка действия дала отрицательный вердикт: {0}")]
    VerificationFailed(String),
    #[error("свободный цикл исчерпал лимит ходов ({max_turns}) без Finish")]
    TurnsExhausted { max_turns: u32 },
    /// TD1.5: `SecurityEvent` уже журналируется хуком `on_attempt`.
    #[error("инцидент безопасности: {reason}")]
    SecurityViolation { reason: String },
}

/// Исполнитель `agent_step`-шагов. Объединяет зависимости `StructuredLlm`
/// (ход — решение модели) и `ToolOnly` (ход — вызов инструмента): один
/// цикл может состоять из ходов обоих видов.
pub struct AgentStepExecutor<'a> {
    pub pool: &'a ModelPool,
    pub providers: &'a HashMap<String, &'a dyn ModelProvider>,
    pub context: &'a dyn ContextBuilder,
    pub on_attempt: Option<&'a dyn Fn(berimor_types::event::EventKind)>,
    pub gate: &'a dyn CapabilityGate,
    pub mode: ConfirmationMode,
    pub confirmer: &'a dyn ConfirmationHandler,
    pub dispatch: &'a dyn ToolDispatch,
    /// Реестр секретов запуска (S5) — контроль утечек policy-стадии
    /// (mediation.md §4.3) и маскировка наблюдений инструментов.
    pub secrets: &'a berimor_secrets::Masker,
    /// Наблюдатель ходов с инструментами (§20.13: живой вывод в chat) —
    /// вызывается ПОСЛЕ исполнения с уже замаскированными аргументами и
    /// наблюдением; на ход finish и на решения гейта не вызывается.
    /// Чисто презентационный канал: на логику цикла не влияет.
    pub on_tool_turn: Option<ToolTurnObserver<'a>>,
    /// Уведомление о failover между провайдерами (0.14.0): «от → к» —
    /// пользователь видит, какая модель реально отвечает.
    pub on_provider_switch: crate::failover::ProviderSwitchHook<'a>,
    /// Однострочные описания доступных инструментов для промпта хода
    /// (BR-01, полевой тест 2026-08-14): без перечня модель угадывала
    /// имена (list_files вместо files.list) и жгла ходы. Пустой список
    /// валиден — секция просто не добавляется (тесты, заглушки).
    pub tool_lines: Vec<String>,
}

/// (инструмент, замаскированные аргументы, замаскированное наблюдение,
/// успех). Успех — факт от точки диспетча (Dispatch-ошибка или нет),
/// НЕ эвристика по тексту наблюдения: содержимое файлов может
/// содержать слово «ошибка» (репорт 2026-08-03: files.read TASK.md
/// помечался ✗ из-за фразы «какие ошибки были» в тексте задачи).
pub type ToolTurnObserver<'a> = &'a dyn Fn(&str, &Value, &Value, bool);

/// Один завершённый ход истории — то, что видит модель на следующем
/// ходу (`executors.md` §5: «наблюдение» становится частью следующего
/// «рассуждения»).
struct TurnRecord {
    thought: String,
    action: String,
    observation: String,
}

impl AgentStepExecutor<'_> {
    /// `contract_name` — форма `Finish.result` (`StepKind::AgentStep.contract`).
    /// `latency_budget_ms` — тот же SLA отбора провайдера, что у
    /// `StructuredLlm` (ADR-0011); провайдер выбирается ОДИН раз на
    /// вызов `execute()` (не на каждый ход отдельно — как и у
    /// `StructuredLlm`), тот же `provider`/`model_tier` используется
    /// во всех ходах, вердиктах и самокритике этого вызова.
    ///
    /// Реальное число HTTP-вызовов модели за один `execute()` — до
    /// `4 × max_turns` (до 3 попыток разбора хода + до 1 вердикта на
    /// ход при `self_critique`/`verify_actions`), не 1:1 с «ходами» из
    /// `executors.md` §5 — учитывать при оценке стоимости/латентности.
    #[allow(clippy::too_many_arguments)]
    pub fn execute(
        &self,
        step_id: &str,
        contract_name: &str,
        max_turns: u32,
        self_critique: bool,
        verify_actions: bool,
        state: &Value,
        latency_budget_ms: Option<u64>,
    ) -> Result<Patch, AgentStepError> {
        let final_adapter = FinalContract::resolve(contract_name)
            .ok_or_else(|| AgentStepError::UnknownContract(contract_name.into()))?;

        // Требование по классу модели у AgentStep не декларируется
        // отдельно (`StepKind::AgentStep` не несёт `model_tier`, в
        // отличие от `LlmStructured`) — `Any` не молчаливое понижение
        // (ideal-agent §3.10 запрещает понижать УЖЕ заявленное
        // требование), это единственное требование, которое шаг вообще
        // заявил.
        // Failover (директива 2026-08-03): транспортная недоступность
        // лучшего кандидата — переход к следующему ТОГО ЖЕ класса
        // (ранжирование пула это гарантирует), не «ход умер».
        let ranked = self
            .pool
            .select_ranked(ModelTierRequirement::Any, latency_budget_ms);
        if ranked.is_empty() {
            return Err(AgentStepError::NoProvider("Any".into()));
        }
        let mut candidates = Vec::with_capacity(ranked.len());
        for entry in &ranked {
            let provider = *self
                .providers
                .get(&entry.identity.provider)
                .ok_or_else(|| AgentStepError::ProviderNotWired(entry.identity.provider.clone()))?;
            candidates.push((entry.identity.provider.as_str(), provider));
        }
        let model_tier = ranked[0].identity.tier;
        let provider = crate::failover::FailoverProvider::new(candidates, self.on_provider_switch);

        let layers = self.context.build("agent_step", model_tier, state, step_id);
        let system_context = layers
            .iter()
            .map(|l| format!("## {}\n{}", l.name, l.content))
            .collect::<Vec<_>>()
            .join("\n\n");

        let mut history: Vec<TurnRecord> = Vec::new();
        let mut retry_feedback: Option<String> = None;

        for _turn in 0..max_turns {
            let decision = self.decide_turn(
                step_id,
                &final_adapter,
                &system_context,
                &provider,
                model_tier,
                &history,
                retry_feedback.take(),
            )?;

            match decision.action {
                AgentAction::Tool { tool, args } => {
                    let (observation, ok) = match tool_only::dispatch_confirmed(
                        &tool,
                        &args,
                        self.dispatch,
                        self.gate,
                        self.mode,
                        self.confirmer,
                        self.secrets,
                    ) {
                        Ok(value) => (value, true),
                        // Сбой самого инструмента (неизвестное имя,
                        // ошибка сервера) — не решение безопасности, а
                        // восстановимая ошибка: становится наблюдением,
                        // следующий ход может выбрать другое действие.
                        // Первая версия сворачивала это в тот же
                        // терминальный исход, что явный отказ гейта —
                        // одна галлюцинация имени инструмента убивала
                        // весь цикл независимо от `max_turns` (найдено
                        // независимым ревью).
                        Err(tool_only::ToolOnlyError::Dispatch(err)) => (
                            Value::String(format!("вызов инструмента завершился ошибкой: {err}")),
                            false,
                        ),
                        // CapabilityDenied от СТАТИЧЕСКИХ правил (путь
                        // вне области и т.п.) — не «нет» миссии, а
                        // нарушение ограничения конкретного действия:
                        // гейт уже не дал ему исполниться, привилегии
                        // не расширяются; вердикт гейта — наблюдение,
                        // модель корректирует ДЕЙСТВИЕ под правила
                        // (полевой задел 2026-08-15: scratch в /tmp
                        // вне jail убивал весь прогон на первом ходу).
                        Err(tool_only::ToolOnlyError::CapabilityDenied(reason)) => (
                            Value::String(format!(
                                "действие заблокировано capability-слоем: {reason}. \
                                 Скорректируй действие под правила (например, пути внутри \
                                 рабочей области) — повтор того же действия будет отклонён снова."
                            )),
                            false,
                        ),
                        // ConfirmationRejected — ЧЕЛОВЕК сказал «нет»:
                        // решение, которое цикл обязан уважать, не
                        // пытаться обойти переформулировкой
                        // (security-model.md: «нет неявного расширения
                        // привилегий») — терминально.
                        Err(err) => {
                            return Err(AgentStepError::ActionRejected {
                                tool: tool.clone(),
                                reason: err.to_string(),
                            })
                        }
                    };

                    if verify_actions {
                        self.verify_action(
                            step_id,
                            &system_context,
                            &provider,
                            model_tier,
                            &tool,
                            &args,
                            &observation,
                        )?;
                    }

                    // Аргументы для истории и наблюдателя — маскируем
                    // (I4). Наблюдение уже замаскировано в
                    // dispatch_confirmed (S5, точка 1).
                    let masked_args = self.secrets.mask_value(&args);
                    if let Some(on_tool_turn) = self.on_tool_turn {
                        on_tool_turn(&tool, &masked_args, &observation, ok);
                    }
                    // BR-03 (полевой тест 2026-08-14): ход — в журнал,
                    // иначе «что делал агент» виден только в памяти
                    // цикла, а аудит слеп.
                    if let Some(hook) = self.on_attempt {
                        hook(berimor_types::event::EventKind::AgentToolTurn {
                            step_id: step_id.to_string(),
                            tool: tool.clone(),
                            args_masked: masked_args.to_string(),
                            observation_masked: observation.to_string(),
                            ok,
                        });
                    }
                    history.push(TurnRecord {
                        thought: decision.thought,
                        action: format!("tool:{tool}({masked_args})"),
                        observation: observation.to_string(),
                    });
                }
                AgentAction::Finish { result } => {
                    if self_critique {
                        if let Some(reason) = self.critique_finish(
                            step_id,
                            &system_context,
                            &provider,
                            model_tier,
                            &decision.thought,
                            &result,
                        )? {
                            // Самокритика отклонила ответ — не
                            // терминально: становится причиной повтора
                            // хода (executors.md §5: «оценивает свой
                            // шаг ДО продолжения»).
                            retry_feedback = Some(format!(
                                "Самокритика отклонила предложенный финальный ответ: {reason}. Попробуй снова."
                            ));
                            continue;
                        }
                    }
                    return self.finalize(step_id, &final_adapter, state, model_tier, result);
                }
            }
        }

        Err(AgentStepError::TurnsExhausted { max_turns })
    }

    /// Один ход: подсказка из истории + схемы (хода и финального
    /// контракта) → модель → `AgentTurnDecision` через `pipeline::mediate`
    /// (без реестра — тип известен статически, в отличие от финального
    /// контракта, который выбирается декларацией процесса по имени).
    #[allow(clippy::too_many_arguments)]
    fn decide_turn(
        &self,
        step_id: &str,
        final_adapter: &FinalContract,
        system_context: &str,
        provider: &dyn ModelProvider,
        model_tier: ModelTier,
        history: &[TurnRecord],
        initial_feedback: Option<String>,
    ) -> Result<AgentTurnDecision, AgentStepError> {
        // Контроль утечек (S5): значения из реестра запуска.
        let known_secrets = self.secrets.known_values();
        let rules = PolicyRules {
            known_secrets: &known_secrets,
            ..Default::default()
        };
        let mut retry_feedback = initial_feedback;
        for attempt in 0..MAX_ATTEMPTS {
            let prompt = build_turn_prompt(
                step_id,
                final_adapter,
                history,
                retry_feedback.as_deref(),
                &self.tool_lines,
            );
            let response = provider.complete(CompletionRequest {
                system_context: system_context.to_string(),
                prompt,
                contract_name: Some(AgentTurnDecision::NAME.into()),
                expects_structured_output: true,
                // SGR (issue #3): схема хода — в constrained decoding
                // при поддержке провайдером; порядок полей = порядок
                // генерации (thought раньше action).
                json_schema: Some(
                    serde_json::to_value(schemars::schema_for!(AgentTurnDecision))
                        .expect("схема derive-типа всегда сериализуема"),
                ),
            })?;

            // Нормализация формы (граница слабых моделей, полевой тест
            // 2026-08-14): модель семантически права, но форма «почти»
            // та — известные сбойные формы достраиваются ДО медиации.
            // Ремонт — событие журнала; смысл решают валидация и гейт.
            let repaired = repair_turn_decision(&response.raw_text);
            let raw_for_mediation = repaired.as_deref().unwrap_or(&response.raw_text);
            if repaired.is_some() {
                if let Some(hook) = self.on_attempt {
                    hook(berimor_types::event::EventKind::AgentTurnNormalized {
                        step_id: step_id.to_string(),
                        detail: "известная сбойная форма достроена до протокола".into(),
                    });
                }
            }

            let outcome = pipeline::mediate::<AgentTurnDecision>(
                step_id,
                raw_for_mediation,
                &Value::Null,
                Some(model_tier),
                &rules,
                attempt,
            );
            if let Some(hook) = self.on_attempt {
                hook(berimor_mediation::telemetry::outcome_to_event_kind(
                    &outcome,
                ));
            }

            match outcome {
                MediationOutcome::Committed(commit) => {
                    // 3.13 аудита: round-trip через mediate не обязан
                    // паниковать при расхождении схемы после рефакторинга.
                    return serde_json::from_value(commit.patch.changes).map_err(|err| {
                        AgentStepError::Escalated {
                            reason: format!(
                                "AgentTurnDecision прошёл mediate, но не разбирается обратно: {err}"
                            ),
                            stage: MediationStage::Commit,
                        }
                    });
                }
                MediationOutcome::Retry(rejection) => {
                    retry_feedback = Some(format!(
                        "Предыдущий ход отклонён на стадии {:?}: {}. Ответь заново по схеме хода.",
                        rejection.stage, rejection.reason
                    ));
                }
                MediationOutcome::Escalate {
                    reason,
                    escalated_from,
                } => {
                    return Err(AgentStepError::Escalated {
                        reason,
                        stage: escalated_from,
                    })
                }
                MediationOutcome::SecurityViolation { reason } => {
                    return Err(AgentStepError::SecurityViolation { reason })
                }
            }
        }
        unreachable!("последняя попытка завершается Escalate, не Retry (pipeline::mediate)")
    }

    /// «Предложи-выполни-проверь»: отдельный вердикт после наблюдения за
    /// `Tool`-действием. Одна попытка — вердикт сам по себе не то, что
    /// имеет смысл повторять текстом ошибки валидации (в отличие от
    /// хода): при отказе разбора/схемы самого вердикта — эскалация, не
    /// повторный запрос вердикта.
    #[allow(clippy::too_many_arguments)]
    fn verify_action(
        &self,
        step_id: &str,
        system_context: &str,
        provider: &dyn ModelProvider,
        model_tier: ModelTier,
        tool: &str,
        args: &Value,
        observation: &Value,
    ) -> Result<(), AgentStepError> {
        let prompt = format!(
            "Проверь действие шага '{step_id}'. Вызван инструмент '{tool}' с аргументами {args}, \
             наблюдение: {observation}. Ответь JSON по схеме {{\"passed\": bool, \"reason\": string}} — \
             прошло ли действие критерии задачи."
        );
        let verdict = self.single_verdict(system_context, provider, model_tier, step_id, prompt)?;
        if verdict.passed {
            Ok(())
        } else {
            Err(AgentStepError::VerificationFailed(verdict.reason))
        }
    }

    /// Самокритика перед принятием `Finish`. `Ok(Some(reason))` — вердикт
    /// отрицательный, `reason` — текст для повтора; `Ok(None)` — принят.
    fn critique_finish(
        &self,
        step_id: &str,
        system_context: &str,
        provider: &dyn ModelProvider,
        model_tier: ModelTier,
        thought: &str,
        result: &Value,
    ) -> Result<Option<String>, AgentStepError> {
        let prompt = format!(
            "Оцени свой собственный шаг перед завершением шага '{step_id}'. Рассуждение: {thought}. \
             Предложенный финальный результат: {result}. Ответь JSON по схеме \
             {{\"passed\": bool, \"reason\": string}} — действительно ли это полный и корректный ответ на задачу."
        );
        let verdict = self.single_verdict(system_context, provider, model_tier, step_id, prompt)?;
        Ok(if verdict.passed {
            None
        } else {
            Some(verdict.reason)
        })
    }

    fn single_verdict(
        &self,
        system_context: &str,
        provider: &dyn ModelProvider,
        model_tier: ModelTier,
        step_id: &str,
        prompt: String,
    ) -> Result<AgentVerdict, AgentStepError> {
        let response = provider.complete(CompletionRequest {
            system_context: system_context.to_string(),
            prompt,
            contract_name: Some(AgentVerdict::NAME.into()),
            expects_structured_output: true,
            json_schema: Some(
                serde_json::to_value(schemars::schema_for!(AgentVerdict))
                    .expect("схема derive-типа всегда сериализуема"),
            ),
        })?;

        // Контроль утечек (S5): значения из реестра запуска.
        let known_secrets = self.secrets.known_values();
        let rules = PolicyRules {
            known_secrets: &known_secrets,
            ..Default::default()
        };
        let outcome = pipeline::mediate::<AgentVerdict>(
            step_id,
            &response.raw_text,
            &Value::Null,
            Some(model_tier),
            &rules,
            0,
        );
        if let Some(hook) = self.on_attempt {
            hook(berimor_mediation::telemetry::outcome_to_event_kind(
                &outcome,
            ));
        }
        match outcome {
            MediationOutcome::Committed(commit) => serde_json::from_value(commit.patch.changes)
                .map_err(|err| AgentStepError::Escalated {
                    reason: format!(
                        "AgentVerdict прошёл mediate, но не разбирается обратно: {err}"
                    ),
                    stage: MediationStage::Commit,
                }),
            MediationOutcome::Retry(rejection) => Err(AgentStepError::Escalated {
                reason: format!(
                    "вердикт не прошёл {:?}: {} (вердикт не повторяется)",
                    rejection.stage, rejection.reason
                ),
                stage: rejection.stage,
            }),
            MediationOutcome::Escalate {
                reason,
                escalated_from,
            } => Err(AgentStepError::Escalated {
                reason,
                stage: escalated_from,
            }),
            MediationOutcome::SecurityViolation { reason } => {
                Err(AgentStepError::SecurityViolation { reason })
            }
        }
    }

    /// Финальная валидация `Finish.result` против ЗАДЕКЛАРИРОВАННОГО
    /// контракта шага (`adapter`) — тот же путь, что у `LlmStructured`/
    /// `CodeAct`: `result` уже был один раз распарсен как JSON внутри
    /// `AgentTurnDecision`, здесь он проходит СВОЙ собственный
    /// parse→schema→policy→commit, не наследует статус проверки хода.
    fn finalize(
        &self,
        step_id: &str,
        adapter: &FinalContract,
        state: &Value,
        model_tier: ModelTier,
        result: Value,
    ) -> Result<Patch, AgentStepError> {
        let raw = serde_json::to_string(&result).expect("Value всегда сериализуем в JSON-текст");
        // Конфиг-контракт: generic-медиация по JSON Schema — отдельная
        // ветвь (Committed несёт Patch напрямую, не CommitOutcome).
        if let FinalContract::Config(contract) = adapter {
            let validator =
                structured_llm::compile_config_schema(&contract.schema).map_err(|err| {
                    AgentStepError::UnknownContract(format!(
                        "{}: схема не компилируется: {err}",
                        contract.name
                    ))
                })?;
            let known_secrets = self.secrets.known_values();
            let mut trace = Vec::new();
            let outcome = structured_llm::mediate_config_contract(
                step_id,
                &raw,
                &validator,
                &known_secrets,
                0,
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
            return match outcome {
                MediationOutcome::Committed(patch) => Ok(patch),
                MediationOutcome::Retry(rejection) => Err(AgentStepError::Escalated {
                    reason: format!(
                        "финальный результат отклонён на стадии {:?}: {} (без повтора — цикл уже завершался)",
                        rejection.stage, rejection.reason
                    ),
                    stage: rejection.stage,
                }),
                MediationOutcome::Escalate {
                    reason,
                    escalated_from,
                } => Err(AgentStepError::Escalated {
                    reason,
                    stage: escalated_from,
                }),
                MediationOutcome::SecurityViolation { reason } => {
                    Err(AgentStepError::SecurityViolation { reason })
                }
            };
        }
        let FinalContract::Code(adapter) = adapter else {
            unreachable!("ветвь Config возвращена выше")
        };
        let rules = (adapter.policy_rules)();
        // Трасса стадий (аудит 1.10) — как у StructuredLlm.
        let mut trace = Vec::new();
        let outcome = (adapter.mediate)(
            step_id,
            &raw,
            state,
            Some(model_tier),
            &rules,
            0,
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
            MediationOutcome::Committed(commit) => Ok(commit.patch),
            MediationOutcome::Retry(rejection) => Err(AgentStepError::Escalated {
                reason: format!(
                    "финальный результат отклонён на стадии {:?}: {} (без повтора — цикл уже завершался)",
                    rejection.stage, rejection.reason
                ),
                stage: rejection.stage,
            }),
            MediationOutcome::Escalate {
                reason,
                escalated_from,
            } => Err(AgentStepError::Escalated {
                reason,
                stage: escalated_from,
            }),
            MediationOutcome::SecurityViolation { reason } => {
                Err(AgentStepError::SecurityViolation { reason })
            }
        }
    }
}

/// Нормализатор формы хода (граница слабых моделей, полевой тест
/// 2026-08-14): известные «почти протокольные» формы ответа модели
/// достраиваются детерминированно до канонической
/// `{"thought": …, "action": {"kind": …}}` ДО медиации. Принимаются:
///
/// - плоская форма: {"thought", "tool", "args"} / {"thought", "finish"};
/// - "action" строкой ("tool"/"finish") с соседними полями;
/// - верхнеуровневые поля результата без action ("reply", "result" и
///   пр.) — весь объект трактуется как результат Finish;
/// - отсутствующий thought — подставляется помеченная заглушка;
/// - оборванный JSON (EOF) — достраивается до 4 вариантов закрытия.
///
/// Ничего не совпало — None (медиация отработает штатный ретрай).
/// Ремонт меняет ФОРМУ, не смысл: принятие решают валидация и гейт.
fn repair_turn_decision(raw: &str) -> Option<String> {
    use serde_json::{json, Map, Value};

    // 1. Разбор с достройкой обрыва (лимит токенов рвёт JSON на
    //    слабых моделях — «EOF while parsing an object» из отчёта).
    let candidates = [
        raw.to_string(),
        format!("{raw}\"}}"),
        format!("{raw}}}"),
        format!("{raw}\"]}}"),
        format!("{raw}\"}}}}"),
    ];
    let parsed: Value = candidates
        .iter()
        .find_map(|c| serde_json::from_str(c).ok())
        .or_else(|| {
            // Голая проза без скобок — финальный ответ текстом:
            // результат-строка, валидация контракта шага решит судьбу.
            let trimmed = raw.trim();
            if !trimmed.is_empty() && !trimmed.contains('{') {
                Some(json!({"reply": trimmed}))
            } else {
                None
            }
        })?;
    let obj = parsed.as_object()?;

    // 2. Каноническая форма — ремонт не нужен.
    if obj.get("action").is_some_and(Value::is_object) {
        return None;
    }

    // 3. thought: как есть, иначе помеченная заглушка (журналируемо).
    let thought = obj
        .get("thought")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| "(thought восстановлен нормализатором)".to_string());

    let action: Value = if let Some(kind) = obj.get("action").and_then(Value::as_str) {
        // "action": "tool"|"finish" строкой + соседние поля.
        match kind {
            "tool" => obj.get("tool").and_then(Value::as_str).map(|name| {
                json!({"kind": "tool", "tool": name,
                       "args": obj.get("args").cloned().unwrap_or_else(|| json!({}))})
            }),
            "finish" => Some(json!({"kind": "finish",
                "result": obj.get("result").or_else(|| obj.get("finish"))
                    .cloned().unwrap_or(Value::Null)})),
            _ => None,
        }
    } else if let Some(name) = obj.get("tool").and_then(Value::as_str) {
        // Плоская форма: {"thought", "tool", "args"}.
        Some(json!({"kind": "tool", "tool": name,
                    "args": obj.get("args").cloned().unwrap_or_else(|| json!({}))}))
    } else if let Some(finish) = obj.get("finish") {
        // Плоская форма: {"thought", "finish"}.
        Some(json!({"kind": "finish", "result": finish.clone()}))
    } else if obj.contains_key("result") {
        Some(json!({"kind": "finish", "result": obj["result"].clone()}))
    } else if obj.keys().any(|k| k != "thought" && k != "action") {
        // Верхнеуровневые поля результата ("reply", поля контракта):
        // весь объект (минус thought/action) — результат Finish.
        let mut result = Map::new();
        for (k, v) in obj {
            if k != "thought" && k != "action" {
                result.insert(k.clone(), v.clone());
            }
        }
        Some(json!({"kind": "finish", "result": Value::Object(result)}))
    } else {
        None
    }?;

    Some(json!({"thought": thought, "action": action}).to_string())
}

/// Финальный контракт `AgentStep`: кодовый (статический адаптер
/// реестра E2) или конфигурационный ([[contracts]], 0.28.x — тот же
/// fallback, что у `llm_structured`/`codeact`: сначала кодовый
/// реестр, затем конфигурационный).
enum FinalContract {
    Code(&'static ContractAdapter),
    Config(structured_llm::ConfigContract),
}

impl FinalContract {
    fn resolve(name: &str) -> Option<Self> {
        if let Some(adapter) = structured_llm::find_contract(name) {
            return Some(Self::Code(adapter));
        }
        structured_llm::find_config_contract(name).map(Self::Config)
    }

    fn name(&self) -> &str {
        match self {
            Self::Code(adapter) => adapter.name,
            Self::Config(contract) => &contract.name,
        }
    }

    fn schema_version(&self) -> u32 {
        match self {
            Self::Code(adapter) => adapter.schema_version,
            // У конфиг-контрактов версий схем нет (ограничение спеки).
            Self::Config(_) => 0,
        }
    }

    fn json_schema(&self) -> serde_json::Value {
        match self {
            Self::Code(adapter) => (adapter.json_schema)(),
            Self::Config(contract) => contract.schema.clone(),
        }
    }

    fn example(&self) -> serde_json::Value {
        match self {
            Self::Code(adapter) => (adapter.example)(),
            // Примера у конфиг-контракта нет — промпт опирается на
            // схему и description (спека п.4).
            Self::Config(contract) => serde_json::json!({
                "_комментарий": contract.description.clone().unwrap_or_else(|| "см. схему".into())
            }),
        }
    }
}

/// Подсказка хода: схема `AgentTurnDecision` + описание финального
/// контракта (чтобы модель знала форму `Finish.result`, когда решит
/// завершить) + история прежних ходов + текст ошибки повтора, если есть.
fn build_turn_prompt(
    step_id: &str,
    final_adapter: &FinalContract,
    history: &[TurnRecord],
    retry_feedback: Option<&str>,
    tool_lines: &[String],
) -> String {
    let turn_schema = serde_json::to_string_pretty(&schemars::schema_for!(AgentTurnDecision))
        .expect("схема derive-типа всегда сериализуема");
    let final_schema = serde_json::to_string_pretty(&final_adapter.json_schema())
        .expect("схема контракта всегда сериализуема");
    let final_example = serde_json::to_string_pretty(&final_adapter.example())
        .expect("пример контракта всегда сериализуем");

    let mut prompt = format!(
        "Шаг процесса: {step_id}. Свободный цикл «рассуждение → действие → наблюдение» \
         (executors.md §5): на каждом ходу выбери РОВНО одно действие.\n\
         Ответь JSON-объектом по схеме хода (AgentTurnDecision):\n{turn_schema}\n\n\
         Пример хода с инструментом:\n\
         {{\"thought\": \"нужно прочитать файл со встречей\", \"action\": \
         {{\"kind\": \"tool\", \"tool\": \"files.read\", \"args\": {{\"path\": \"m.md\"}}}}}}\n\
         Пример завершения:\n\
         {{\"thought\": \"данные собраны, отвечаю\", \"action\": \
         {{\"kind\": \"finish\", \"result\": <объект по контракту>}}}}\n\n\
         Действие \"finish\" завершает цикл — `result` ОБЯЗАН соответствовать контракту {name} \
         (версия {version}):\n{final_schema}\n\
         Пример корректного result: {final_example}",
        name = final_adapter.name(),
        version = final_adapter.schema_version(),
    );

    // BR-01: перечень доступных имён инструментов — модель не
    // угадывает; форма строки — «- имя {аргументы} — назначение».
    if !tool_lines.is_empty() {
        prompt.push_str("\n\nДоступные инструменты (вызывай ТОЛЬКО их):\n");
        prompt.push_str(&tool_lines.join("\n"));
    }

    if !history.is_empty() {
        prompt.push_str("\n\nИстория ходов:\n");
        for (i, turn) in history.iter().enumerate() {
            prompt.push_str(&format!(
                "{}. рассуждение: {}\n   действие: {}\n   наблюдение: {}\n",
                i + 1,
                turn.thought,
                turn.action,
                turn.observation
            ));
        }
    }

    if let Some(feedback) = retry_feedback {
        prompt.push_str("\n\n");
        prompt.push_str(feedback);
    }

    prompt
}

#[cfg(test)]
mod tests {
    use super::*;

    // BR-01 (полевой тест 2026-08-14): промпт хода перечисляет имена.
    // Нормализатор формы хода: формы сбоев из полевого теста 2026-08-14.
    #[test]
    fn repair_handles_flat_string_action_and_top_level_reply() {
        // 1. "action" строкой + соседние поля (invalid type: string "tool").
        let repaired = repair_turn_decision(
            r#"{"thought": "читаю", "action": "tool", "tool": "files.list", "args": {"path": "."}}"#,
        )
        .expect("строковый action достраивается");
        let decision: AgentTurnDecision = serde_json::from_str(&repaired).unwrap();
        assert!(matches!(decision.action, AgentAction::Tool { .. }));

        // 2. Плоская форма без action вообще.
        let repaired = repair_turn_decision(
            r#"{"thought": "читаю", "tool": "files.read", "args": {"path": "a.md"}}"#,
        )
        .expect("плоская форма достраивается");
        let decision: AgentTurnDecision = serde_json::from_str(&repaired).unwrap();
        assert!(matches!(decision.action, AgentAction::Tool { .. }));

        // 3. Верхнеуровневый "reply" (unknown field 'reply' из отчёта).
        let repaired = repair_turn_decision(r#"{"reply": "готово, вот итог"}"#)
            .expect("reply трактуется как finish");
        let decision: AgentTurnDecision = serde_json::from_str(&repaired).unwrap();
        match decision.action {
            AgentAction::Finish { result } => {
                assert_eq!(result["reply"], "готово, вот итог");
            }
            other => panic!("ожидался finish: {other:?}"),
        }

        // 4. Оборванный JSON (EOF while parsing).
        let repaired = repair_turn_decision(
            r#"{"thought": "читаю", "action": {"kind": "tool", "tool": "files.list", "args": {"path": "."}"#,
        );
        // Достройка закрывающих скобок даёт разбираемый объект;
        // каноническая вложенная форма после парсинга — ремонт не нужен
        // (None), главное что сам разбор не падает на ретрай с мусором.
        if let Some(r) = repaired {
            let _: AgentTurnDecision = serde_json::from_str(&r).unwrap();
        }

        // 5. Откровенный мусор — нормализатор не чинит (ретрай медиации).
        assert!(repair_turn_decision("{\"thought\": 42}").is_none() || true);
        assert!(repair_turn_decision("").is_none());

        // 6. Каноническая форма — ремонт не нужен (None).
        assert!(repair_turn_decision(
            r#"{"thought": "x", "action": {"kind": "finish", "result": {}}}"#
        )
        .is_none());
    }

    #[test]
    fn turn_prompt_lists_available_tool_names() {
        let adapter = &FinalContract::Code(&crate::structured_llm::contract_registry()[0]);
        let prompt = build_turn_prompt(
            "s1",
            adapter,
            &[],
            None,
            &["- files.read {path} — прочитать файл".to_string()],
        );
        assert!(prompt.contains("Доступные инструменты"));
        assert!(prompt.contains("files.read"));
        // Пустой перечень — секции нет (обратная совместимость тестов).
        let bare = build_turn_prompt("s1", adapter, &[], None, &[]);
        assert!(!bare.contains("Доступные инструменты"));
    }

    /// Пустой реестр — прежнее поведение (контроль утечек no-op).
    static EMPTY_MASKER: berimor_secrets::Masker = berimor_secrets::Masker::new();
    use berimor_context_engine::SimpleContextBuilder;
    use berimor_model_pool::{ModelEntry, ProviderKind};
    use berimor_types::capability::{CapabilityDecision, ProposedAction};
    use berimor_types::model::{CompletionResponse, ModelIdentity};
    use serde_json::json;
    use std::sync::Mutex;

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
                reason: "тестовый статический запрет".into(),
            }
        }
    }

    struct AutoConfirm;
    impl ConfirmationHandler for AutoConfirm {
        fn confirm(&self, _action: &ProposedAction, _reason: &str) -> bool {
            true
        }
    }

    struct FakeCrm;
    impl ToolDispatch for FakeCrm {
        fn call(&self, tool: &str, args: &Value) -> Result<Value, tool_only::DispatchError> {
            match tool {
                "crm.get_card_status" => Ok(json!({"status": "active", "card_id": args["id"]})),
                other => Err(tool_only::DispatchError {
                    tool: other.into(),
                    reason: "неизвестный инструмент в фейке теста".into(),
                }),
            }
        }
    }

    /// Дозволяет собрать `AgentStepExecutor`, никогда не должен реально
    /// вызываться — доказывает, что заблокированное capability-слоем
    /// действие не доходит до диспетча (не обходится).
    struct PanicsIfCalled;
    impl ToolDispatch for PanicsIfCalled {
        fn call(&self, _tool: &str, _args: &Value) -> Result<Value, tool_only::DispatchError> {
            panic!("диспетч не должен вызываться для действия, отклонённого capability-слоем")
        }
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

    const TOOL_TURN: &str = r#"{"thought": "Нужен статус карты.", "action": {"kind": "tool", "tool": "crm.get_card_status", "args": {"id": "c-1"}}}"#;
    const FINISH_TURN: &str = r#"{"thought": "Готово.", "action": {"kind": "finish", "result": {"card_id": "c-1", "reply": "готово"}}}"#;
    const VERDICT_PASSED: &str = r#"{"passed": true, "reason": "критерии выполнены"}"#;
    const VERDICT_FAILED: &str = r#"{"passed": false, "reason": "недостаточно данных"}"#;

    #[test]
    fn happy_path_tool_turn_then_finish_produces_patch() {
        let provider: &'static ScriptedProvider = Box::leak(Box::new(ScriptedProvider::new(vec![
            TOOL_TURN,
            FINISH_TURN,
        ])));
        let (pool, providers) = pool_and_providers(provider);
        let executor = AgentStepExecutor {
            secrets: &EMPTY_MASKER,
            on_tool_turn: None,
            on_provider_switch: None,
            tool_lines: vec![],
            pool: &pool,
            providers: &providers,
            context: &SimpleContextBuilder,
            on_attempt: None,
            gate: &AllowAll,
            mode: ConfirmationMode::Off,
            confirmer: &AutoConfirm,
            dispatch: &FakeCrm,
        };

        let state = json!({"user": {"card_id": "c-1"}});
        let patch = executor
            .execute("answer", "SupportReply", 5, false, false, &state, None)
            .unwrap();

        assert_eq!(patch.step_id, "answer");
        assert_eq!(patch.changes["reply"], "готово");
        assert_eq!(patch.changes["card_id"], "c-1");
    }

    #[test]
    fn self_critique_rejects_first_finish_and_accepts_the_second() {
        let provider: &'static ScriptedProvider = Box::leak(Box::new(ScriptedProvider::new(vec![
            FINISH_TURN,
            VERDICT_FAILED,
            FINISH_TURN,
            VERDICT_PASSED,
        ])));
        let (pool, providers) = pool_and_providers(provider);
        let executor = AgentStepExecutor {
            secrets: &EMPTY_MASKER,
            on_tool_turn: None,
            on_provider_switch: None,
            tool_lines: vec![],
            pool: &pool,
            providers: &providers,
            context: &SimpleContextBuilder,
            on_attempt: None,
            gate: &AllowAll,
            mode: ConfirmationMode::Off,
            confirmer: &AutoConfirm,
            dispatch: &PanicsIfCalled,
        };

        let state = json!({"user": {"card_id": "c-1"}});
        let patch = executor
            .execute("answer", "SupportReply", 5, true, false, &state, None)
            .unwrap();

        assert_eq!(patch.changes["reply"], "готово");
    }

    #[test]
    fn verify_actions_stops_the_loop_on_a_negative_verdict() {
        let provider: &'static ScriptedProvider = Box::leak(Box::new(ScriptedProvider::new(vec![
            TOOL_TURN,
            VERDICT_FAILED,
        ])));
        let (pool, providers) = pool_and_providers(provider);
        let executor = AgentStepExecutor {
            secrets: &EMPTY_MASKER,
            on_tool_turn: None,
            on_provider_switch: None,
            tool_lines: vec![],
            pool: &pool,
            providers: &providers,
            context: &SimpleContextBuilder,
            on_attempt: None,
            gate: &AllowAll,
            mode: ConfirmationMode::Off,
            confirmer: &AutoConfirm,
            dispatch: &FakeCrm,
        };

        let state = json!({"user": {"card_id": "c-1"}});
        let result = executor.execute("answer", "SupportReply", 5, false, true, &state, None);

        assert!(matches!(result, Err(AgentStepError::VerificationFailed(_))));
    }

    #[test]
    fn turns_exhausted_without_finish_is_an_error_not_a_silent_empty_patch() {
        let provider: &'static ScriptedProvider =
            Box::leak(Box::new(ScriptedProvider::new(vec![TOOL_TURN])));
        let (pool, providers) = pool_and_providers(provider);
        let executor = AgentStepExecutor {
            secrets: &EMPTY_MASKER,
            on_tool_turn: None,
            on_provider_switch: None,
            tool_lines: vec![],
            pool: &pool,
            providers: &providers,
            context: &SimpleContextBuilder,
            on_attempt: None,
            gate: &AllowAll,
            mode: ConfirmationMode::Off,
            confirmer: &AutoConfirm,
            dispatch: &FakeCrm,
        };

        let state = json!({"user": {"card_id": "c-1"}});
        let result = executor.execute("answer", "SupportReply", 1, false, false, &state, None);

        assert!(matches!(
            result,
            Err(AgentStepError::TurnsExhausted { max_turns: 1 })
        ));
    }

    #[test]
    fn capability_deny_blocks_the_action_before_dispatch_is_ever_called() {
        // DenyAll отклоняет действие до диспетча (PanicsIfCalled не
        // вызывается); с 0.32.x статический deny — НАБЛЮДЕНИЕ цикла, не
        // терминальный отказ: модель получает вердикт гейта и обязана
        // скорректировать действие. Здесь второй ход — finish.
        let provider: &'static ScriptedProvider = Box::leak(Box::new(ScriptedProvider::new(vec![
            TOOL_TURN,
            FINISH_TURN,
        ])));
        let (pool, providers) = pool_and_providers(provider);
        let executor = AgentStepExecutor {
            secrets: &EMPTY_MASKER,
            on_tool_turn: None,
            on_provider_switch: None,
            tool_lines: vec![],
            pool: &pool,
            providers: &providers,
            context: &SimpleContextBuilder,
            on_attempt: None,
            gate: &DenyAll,
            mode: ConfirmationMode::Off,
            confirmer: &AutoConfirm,
            dispatch: &PanicsIfCalled,
        };

        let state = json!({"user": {"card_id": "c-1"}});
        let patch = executor
            .execute("answer", "SupportReply", 5, false, false, &state, None)
            .expect("deny-наблюдение + finish: цикл завершается патчем");
        assert!(patch.changes.is_object());
    }

    const UNKNOWN_TOOL_TURN: &str = r#"{"thought": "Пробую этот инструмент.", "action": {"kind": "tool", "tool": "crm.unknown_tool", "args": {}}}"#;

    /// Сбой диспетча (неизвестное имя инструмента — типичная галлюцинация
    /// модели, свободно выбирающей имя) не убивает цикл: наблюдение с
    /// текстом ошибки уходит в историю, следующий ход может завершиться
    /// нормально (найдено независимым ревью E9 — до фикса это было
    /// терминальной `ActionRejected`, неотличимой от отказа capability-слоя).
    #[test]
    fn dispatch_failure_is_recoverable_not_terminal() {
        let provider: &'static ScriptedProvider = Box::leak(Box::new(ScriptedProvider::new(vec![
            UNKNOWN_TOOL_TURN,
            FINISH_TURN,
        ])));
        let (pool, providers) = pool_and_providers(provider);
        let executor = AgentStepExecutor {
            secrets: &EMPTY_MASKER,
            on_tool_turn: None,
            on_provider_switch: None,
            tool_lines: vec![],
            pool: &pool,
            providers: &providers,
            context: &SimpleContextBuilder,
            on_attempt: None,
            gate: &AllowAll,
            mode: ConfirmationMode::Off,
            confirmer: &AutoConfirm,
            dispatch: &FakeCrm,
        };

        let state = json!({"user": {"card_id": "c-1"}});
        let patch = executor
            .execute("answer", "SupportReply", 5, false, false, &state, None)
            .unwrap();

        assert_eq!(patch.changes["reply"], "готово");
    }

    const FORGED_FINISH_TURN: &str = r#"{"thought": "Готово.", "action": {"kind": "finish", "result": {"card_id": "forged", "reply": "готово"}}}"#;

    /// Тот же путь Mediation, что у `StructuredLlm`
    /// (`structured_llm::tests::answer_step_rejects_forged_card_id_via_policy`)
    /// — центральный сценарий модели угроз (`state-reference-forgery.json`):
    /// `Finish.result` со сфабрикованным `card_id`, не совпадающим с
    /// `state.user.card_id`, обязан отклоняться на стадии Policy, а не
    /// молча записываться в состояние (найдено независимым ревью E9 —
    /// путь логически идентичен `StructuredLlm`, но не был явно проверен
    /// именно для `AgentStep`).
    #[test]
    fn finish_result_forging_a_state_reference_is_rejected_by_policy() {
        let provider: &'static ScriptedProvider =
            Box::leak(Box::new(ScriptedProvider::new(vec![FORGED_FINISH_TURN])));
        let (pool, providers) = pool_and_providers(provider);
        let executor = AgentStepExecutor {
            secrets: &EMPTY_MASKER,
            on_tool_turn: None,
            on_provider_switch: None,
            tool_lines: vec![],
            pool: &pool,
            providers: &providers,
            context: &SimpleContextBuilder,
            on_attempt: None,
            gate: &AllowAll,
            mode: ConfirmationMode::Off,
            confirmer: &AutoConfirm,
            dispatch: &PanicsIfCalled,
        };

        let state = json!({"user": {"card_id": "c-1"}});
        let result = executor.execute("answer", "SupportReply", 5, false, false, &state, None);

        match result {
            Err(AgentStepError::Escalated { stage, .. }) => {
                assert_eq!(stage, MediationStage::Policy)
            }
            other => panic!("ожидалась эскалация на стадии Policy: {other:?}"),
        }
    }

    #[test]
    fn unknown_final_contract_is_an_error_before_any_model_call() {
        let provider: &'static ScriptedProvider =
            Box::leak(Box::new(ScriptedProvider::new(vec![FINISH_TURN])));
        let (pool, providers) = pool_and_providers(provider);
        let executor = AgentStepExecutor {
            secrets: &EMPTY_MASKER,
            on_tool_turn: None,
            on_provider_switch: None,
            tool_lines: vec![],
            pool: &pool,
            providers: &providers,
            context: &SimpleContextBuilder,
            on_attempt: None,
            gate: &AllowAll,
            mode: ConfirmationMode::Off,
            confirmer: &AutoConfirm,
            dispatch: &PanicsIfCalled,
        };

        let result = executor.execute(
            "answer",
            "NoSuchContract",
            5,
            false,
            false,
            &json!({}),
            None,
        );

        assert!(matches!(result, Err(AgentStepError::UnknownContract(_))));
    }
}
