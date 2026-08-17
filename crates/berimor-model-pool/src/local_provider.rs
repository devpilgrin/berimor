//! Локальный инференс GGUF через llama.cpp — встраивается в процесс, без
//! отдельного сервера (ADR-0024, `stack.md` §5). ROADMAP: E4.
//!
//! Разделение на две части:
//! - [`LlamaLocalProvider`] — клей к контракту `ModelProvider` (сборка
//!   подсказки из `CompletionRequest`, обёртка ответа, отображение
//!   ошибок). Обобщён по [`GgufEngine`], компилируется и тестируется без
//!   C++-зависимости — тесты идут на фейковом движке;
//! - `LlamaCppEngine` (feature `local-inference`) — реальный бэкенд на
//!   `llama-cpp-2`; собирается только по запросу, т.к. сборка ядра
//!   llama.cpp занимает минуты и не должна быть ценой каждого CI-прогона.
//!
//! Осознанные решения (для ревью):
//! - вызов СИНХРОННЫЙ, как весь стек исполнителей (engine, tool_only,
//!   `OpenAiCompatibleProvider` — блокирующие): llama.cpp-инференс — это
//!   CPU/GPU-вычисление в потоке вызывающего, отдельного рантайма ему не
//!   нужно; временные пределы шага остаются на `ProcessLimits` (P6);
//! - строгость JSON НЕ возлагается на движок: валидность вывода
//!   обеспечивает Mediation (M2/M3 с повторами, M4) — тот же принцип,
//!   что у HTTP-провайдеров без `response_format`; грамматики GBNF —
//!   возможное усиление позже, не часть E4;
//! - модель загружается из пути из конфигурации (I5: веса — локальный
//!   файл, данные не покидают периметр); сетевого гейта здесь нет и не
//!   нужно — сетевых обращений нет в принципе.

use berimor_types::{
    executor::ModelProvider,
    model::{CompletionRequest, CompletionResponse, ModelError, ModelIdentity},
};
#[cfg(test)]
use std::sync::Mutex;

/// Структурированная подсказка для локального движка: провайдер
/// разделяет роли, движок рендерит chat-template модели (свой у каждого
/// семейства — Qwen3 без своего шаблона отвечает think-блоком, не JSON;
/// поймано smoke-прогоном E4) с откатом на нейтральные маркеры ролей.
pub struct LocalPrompt<'a> {
    pub system_context: &'a str,
    pub user: &'a str,
    /// Ожидается JSON по контракту: движок вправе применить prefill
    /// `{` после промпта ассистента и вернуть его в тексте ответа —
    /// дешёвое и надёжное принуждение к JSON-началу для локальных
    /// моделей (у HTTP-провайдера аналог — `response_format`).
    pub expects_json: bool,
}

/// Движок локальной генерации — минимальный интерфейс, который нужен
/// провайдеру. Реализация на llama.cpp — за feature-флагом (см. выше);
/// тесты используют фейк.
pub trait GgufEngine: Send + Sync {
    /// Синхронно, в потоке вызывающего. `grammar` — GBNF (см.
    /// `crate::gbnf`) для принуждения структуры и порядка полей на
    /// семплировании (issue #3), None — свободная генерация (валидация
    /// всё равно на Mediation).
    fn generate(&self, prompt: &LocalPrompt, grammar: Option<&str>) -> Result<String, ModelError>;
}

/// Провайдер Model Pool поверх локального движка. `identity.tier` — класс
/// из паспорта модели при регистрации (ADR-0010), сам провайдер его не
/// вычисляет и не перепроверяет.
pub struct LlamaLocalProvider<E: GgufEngine> {
    identity: ModelIdentity,
    engine: E,
    /// GBNF-принуждение по схеме контракта (response_format =
    /// "json_schema"|"grammar" у локального провайдера, issue #3).
    use_grammar: bool,
}

impl<E: GgufEngine> LlamaLocalProvider<E> {
    pub fn new(identity: ModelIdentity, engine: E, use_grammar: bool) -> Self {
        Self {
            identity,
            engine,
            use_grammar,
        }
    }

    pub fn identity(&self) -> &ModelIdentity {
        &self.identity
    }

    /// Тестовая разборка: забрать движок обратно (проверка, что грамматика
    /// дошла до движка, — через записи фейка).
    #[cfg(test)]
    fn into_engine(self) -> E {
        self.engine
    }

    /// GBNF из схемы запроса — когда принуждение включено конфигом и
    /// схема есть. Конвертер поддерживает подмножество схем: отказ —
    /// предупреждение в stderr и свободная генерация (валидация всё
    /// равно на Mediation, деградация честная и видимая).
    fn grammar_for(&self, request: &CompletionRequest) -> Option<String> {
        if !self.use_grammar || !request.expects_structured_output {
            return None;
        }
        let schema = request.json_schema.as_ref()?;
        match crate::gbnf::json_schema_to_gbnf(schema) {
            Ok(grammar) => Some(grammar),
            Err(reason) => {
                eprintln!(
                    "[berimor] GBNF не построен для '{}': {reason} — свободная генерация",
                    request
                        .contract_name
                        .as_deref()
                        .unwrap_or("<без контракта>")
                );
                None
            }
        }
    }
}

impl<E: GgufEngine> ModelProvider for LlamaLocalProvider<E> {
    fn complete(&self, request: CompletionRequest) -> Result<CompletionResponse, ModelError> {
        // HTTP-провайдер включил бы `response_format: json_object`; у
        // локального движка такого переключателя нет — инструкция в
        // подсказке + prefill + валидация Mediation с повторами (M2/M3/M6).
        let user = match &request.contract_name {
            Some(contract) => format!(
                "{}\n\nОтветь ТОЛЬКО валидным JSON по контракту {contract}, без пояснений и markdown.",
                request.prompt
            ),
            None => request.prompt.clone(),
        };
        let raw_text = self.engine.generate(
            &LocalPrompt {
                system_context: &request.system_context,
                user: &user,
                expects_json: request.expects_structured_output,
            },
            self.grammar_for(&request).as_deref(),
        )?;
        Ok(CompletionResponse {
            raw_text,
            model: self.identity.clone(),
        })
    }
}

/// Реальный бэкенд на llama.cpp — только под feature-флагом (см. шапку).
#[cfg(feature = "local-inference")]
mod llama_backend {
    use super::{GgufEngine, LocalPrompt};
    use berimor_types::model::ModelError;
    use llama_cpp_2::{
        context::params::LlamaContextParams,
        llama_backend::LlamaBackend,
        llama_batch::LlamaBatch,
        model::{params::LlamaModelParams, AddBos, LlamaModel},
        sampling::LlamaSampler,
    };
    use std::num::NonZeroU32;
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    /// Размер контекста (KV-кэш) на один вызов. Дефолт биндингов — 512
    /// токенов: подсказка StructuredLlm (схема + пример + контекст)
    /// больше — реальный прогон на Qwen3-0.6B упал с `NoKvCacheSlot`
    /// (smoke E4). 4 КиБ покрывает подсказки узких шагов с запасом.

    /// Максимум генерируемых токенов на один вызов — защита от бесконечной
    /// генерации (у HTTP-провайдера аналог — таймаут; здесь вычисление
    /// локально, предел — по токенам).
    const MAX_NEW_TOKENS: usize = 4096;

    /// llama.cpp-модель, загруженная один раз на процесс из пути в
    /// конфигурации. Контекст создаётся на каждый вызов — состояние KV не
    /// протекает между шагами процесса (детерминизм инстанса).
    /// Контекст по умолчанию (токены): 8192 — поднято с 4096 по репорту
    /// 2026-08-16 (большой структурный ответ обрывался об потолок —
    /// «EOF while parsing» на эскалации). Переопределяется конфигом
    /// провайдера (`local_ctx_tokens`).
    const DEFAULT_N_CTX: u32 = 8192;

    pub struct LlamaCppEngine {
        model: LlamaModel,
        // `LlamaBackend` инициализируется один раз; держим внутри, чтобы
        // время жизни превышало модель (бэкенд владеет глобальным
        // состоянием llama.cpp).
        _backend: Arc<LlamaBackend>,
        model_path: PathBuf,
        n_ctx: u32,
        // llama.cpp-контекст не `Sync` — сериализуем вызовы внутри
        // провайдера (ModelProvider требует &self-вызовов из разных
        // потоков: Engine запускает параллельные ветки P3).
        lock: Mutex<()>,
    }

    impl LlamaCppEngine {
        pub fn load(backend: Arc<LlamaBackend>, model_path: &Path) -> Result<Self, ModelError> {
            Self::load_with_ctx(backend, model_path, DEFAULT_N_CTX)
        }

        pub fn load_with_ctx(
            backend: Arc<LlamaBackend>,
            model_path: &Path,
            n_ctx: u32,
        ) -> Result<Self, ModelError> {
            let model =
                LlamaModel::load_from_file(&backend, model_path, &LlamaModelParams::default())
                    .map_err(|err| {
                        ModelError::Unavailable(format!(
                            "не удалось загрузить GGUF {}: {err}",
                            model_path.display()
                        ))
                    })?;
            Ok(Self {
                model,
                _backend: backend,
                model_path: model_path.to_path_buf(),
                n_ctx,
                lock: Mutex::new(()),
            })
        }
    }

    use std::sync::Arc;

    /// Позиция ПОСЛЕ закрывающей `}` первого сбалансированного JSON-объекта
    /// в `text`, если объект завершён; стартовая глубина 1 — открывающая
    /// скобка учтена prefill'ом. Скобки внутри строк и `\`-экранирование
    /// не влияют на глубину.
    fn json_balance_end(text: &str) -> Option<usize> {
        let mut depth = 1usize;
        let mut in_string = false;
        let mut escaped = false;
        for (index, ch) in text.char_indices() {
            if escaped {
                escaped = false;
                continue;
            }
            match ch {
                '\\' if in_string => escaped = true,
                '"' => in_string = !in_string,
                '{' | '[' if !in_string => depth += 1,
                '}' | ']' if !in_string => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        return Some(index + ch.len_utf8());
                    }
                }
                _ => {}
            }
        }
        None
    }

    impl GgufEngine for LlamaCppEngine {
        fn generate(
            &self,
            prompt: &LocalPrompt,
            _grammar: Option<&str>,
        ) -> Result<String, ModelError> {
            let _guard = self.lock.lock().expect("мьютекс движка не отравлен");

            let ctx_params = LlamaContextParams::default()
                .with_n_ctx(NonZeroU32::new(self.n_ctx))
                .with_n_batch(self.n_ctx);
            let mut ctx = self
                .model
                .new_context(&self._backend, ctx_params)
                .map_err(|err| {
                    ModelError::Unavailable(format!(
                        "не удалось создать контекст {}: {err}",
                        self.model_path.display()
                    ))
                })?;

            // Chat-template из GGUF — родной формат модели (без него Qwen3
            // отвечает think-блоком и mediation эскалирует на parse:
            // первый smoke E4). Откат — нейтральные маркеры ролей.
            // Prefill `{` при ожидании JSON: модель ПРОДОЛЖАЕТ объект,
            // не изобретает обёртку; префил возвращается в тексте ответа,
            // чтобы parse-стадия видела целый документ.
            let (full_prompt, add_bos) = match self.model.chat_template(None) {
                Ok(template) => {
                    let mut messages = Vec::new();
                    if !prompt.system_context.is_empty() {
                        messages.push(
                            llama_cpp_2::model::LlamaChatMessage::new(
                                "system".to_string(),
                                prompt.system_context.to_string(),
                            )
                            .map_err(|err| ModelError::Unavailable(format!("сообщение: {err}")))?,
                        );
                    }
                    messages.push(
                        llama_cpp_2::model::LlamaChatMessage::new(
                            "user".to_string(),
                            prompt.user.to_string(),
                        )
                        .map_err(|err| ModelError::Unavailable(format!("сообщение: {err}")))?,
                    );
                    let mut rendered = self
                        .model
                        .apply_chat_template(&template, &messages, true)
                        .map_err(|err| {
                        ModelError::Unavailable(format!("chat-template: {err}"))
                    })?;
                    if prompt.expects_json {
                        rendered.push('{');
                    }
                    // Шаблон сам несёт bos/спецтокены — не дублировать.
                    (rendered, AddBos::Never)
                }
                Err(_) => {
                    let mut rendered = String::new();
                    if !prompt.system_context.is_empty() {
                        rendered.push_str("[system]\n");
                        rendered.push_str(prompt.system_context);
                        rendered.push_str("\n\n");
                    }
                    rendered.push_str("[user]\n");
                    rendered.push_str(prompt.user);
                    rendered.push_str("\n[assistant]\n");
                    if prompt.expects_json {
                        rendered.push('{');
                    }
                    (rendered, AddBos::Always)
                }
            };

            let tokens = self
                .model
                .str_to_token(&full_prompt, add_bos)
                .map_err(|err| ModelError::Unavailable(format!("токенизация: {err}")))?;

            // Подсказка длиннее контекста — ОШИБКА, не decode: llama.cpp
            // на превышении n_batch падает GGML_ASSERT'ом (abort всего
            // процесса, из Rust не поймать) — гипотеза независимого
            // ревью E4, подтверждённая их probe. Ошибка — возобновляемая
            // семантика ModelError, не падение агента.
            if tokens.len() >= self.n_ctx as usize {
                return Err(ModelError::Unavailable(format!(
                    "подсказка ({} токенов) превышает контекст ({})",
                    tokens.len(),
                    self.n_ctx
                )));
            }

            let mut batch = LlamaBatch::new(tokens.len().max(512), 1);
            let last = tokens.len().saturating_sub(1);
            for (i, token) in tokens.iter().enumerate() {
                batch
                    .add(*token, i as i32, &[0], i == last)
                    .map_err(|err| ModelError::Unavailable(format!("batch: {err}")))?;
            }
            ctx.decode(&mut batch)
                .map_err(|err| ModelError::Unavailable(format!("decode: {err}")))?;

            // GBNF-грамматика контракта (issue #3): сэмплер грамматики
            // первым в цепочке — структура и порядок полей принуждаются
            // на уровне логитов, не промпта. Ошибка компиляции
            // грамматики — откат на свободную генерацию с
            // предупреждением (валидация всё равно на Mediation).
            let mut samplers = Vec::new();
            if let Some(grammar) = _grammar {
                match LlamaSampler::grammar(&self.model, grammar, "root") {
                    Ok(grammar_sampler) => samplers.push(grammar_sampler),
                    Err(err) => {
                        eprintln!("[berimor] GBNF не принят движком ({err}) — свободная генерация")
                    }
                }
            }
            samplers.push(LlamaSampler::greedy());
            let mut sampler = LlamaSampler::chain_simple(samplers);
            // Потоковый декодер: кусок токена может резать многобайтовый
            // UTF-8-символ — побайтовый from_utf8_lossy на каждый токен
            // такие символы калечит.
            let mut decoder = encoding_rs::UTF_8.new_decoder();
            let mut out = String::new();
            let start_pos = tokens.len();
            let budget = MAX_NEW_TOKENS.min((self.n_ctx as usize).saturating_sub(start_pos));
            for offset in 0..budget {
                let token = sampler.sample(&ctx, batch.n_tokens() - 1);
                if self.model.is_eog_token(token) {
                    break;
                }
                let piece = self
                    .model
                    .token_to_piece(token, &mut decoder, false, None)
                    .map_err(|err| ModelError::Unavailable(format!("детокенизация: {err}")))?;
                out.push_str(&piece);

                // Ожидается JSON: локальная модель после закрывающей `}`
                // продолжает болтать, а parse-стадия (M2) справедливо
                // отвергает trailing characters (второй smoke E4).
                // Останавливаем генерацию на балансе скобок — prefill `{`
                // учтён стартовой глубиной 1. Сканер отслеживает строки и
                // экранирование, скобки внутри JSON-строк глубину не меняют.
                if prompt.expects_json {
                    if let Some(end) = json_balance_end(&out) {
                        out.truncate(end);
                        break;
                    }
                }

                batch.clear();
                batch
                    .add(token, (start_pos + offset) as i32, &[0], true)
                    .map_err(|err| ModelError::Unavailable(format!("batch: {err}")))?;
                ctx.decode(&mut batch)
                    .map_err(|err| ModelError::Unavailable(format!("decode: {err}")))?;
            }
            if prompt.expects_json {
                out.insert(0, '{');
            }
            Ok(out)
        }
    }
}

#[cfg(feature = "local-inference")]
pub use llama_backend::LlamaCppEngine;
/// Реэкспорт типа бэкенда для вызывающего кода (CLI) — crate биндингов
/// остаётся деталью ЭТОГО модуля, а не размазан по workspace.
#[cfg(feature = "local-inference")]
pub use llama_cpp_2::llama_backend::LlamaBackend;

#[cfg(test)]
mod tests {
    use super::*;
    use berimor_types::model::ModelTier;

    /// Фейковый движок: записывает подсказку, возвращает сценарный ответ.
    struct FakeEngine {
        answer: Result<String, String>,
        seen_prompts: Mutex<Vec<(String, String, bool)>>,
        seen_grammars: Mutex<Vec<Option<String>>>,
    }

    impl GgufEngine for FakeEngine {
        fn generate(
            &self,
            prompt: &LocalPrompt,
            grammar: Option<&str>,
        ) -> Result<String, ModelError> {
            self.seen_prompts.lock().unwrap().push((
                prompt.system_context.to_string(),
                prompt.user.to_string(),
                prompt.expects_json,
            ));
            self.seen_grammars
                .lock()
                .unwrap()
                .push(grammar.map(str::to_string));
            match &self.answer {
                Ok(text) => Ok(text.clone()),
                Err(reason) => Err(ModelError::Unavailable(reason.clone())),
            }
        }
    }

    fn provider(engine: FakeEngine) -> LlamaLocalProvider<FakeEngine> {
        provider_with_grammar(engine, true)
    }

    fn provider_with_grammar(
        engine: FakeEngine,
        use_grammar: bool,
    ) -> LlamaLocalProvider<FakeEngine> {
        LlamaLocalProvider::new(
            ModelIdentity {
                provider: "llama-local".to_string(),
                model_id: "qwen3-4b-q4".to_string(),
                tier: ModelTier::Weak,
            },
            engine,
            use_grammar,
        )
    }

    #[test]
    fn grammar_from_schema_reaches_engine_when_enabled() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {"category": {"type": "string"}},
            "required": ["category"]
        });
        let engine = FakeEngine {
            answer: Ok(r#"{"category": "card"}"#.to_string()),
            seen_prompts: Mutex::new(Vec::new()),
            seen_grammars: Mutex::new(Vec::new()),
        };
        let provider = provider_with_grammar(engine, true);
        provider
            .complete(CompletionRequest {
                system_context: String::new(),
                prompt: "x".into(),
                contract_name: Some("C".into()),
                expects_structured_output: true,
                json_schema: Some(schema),
            })
            .unwrap();
        let engine = provider.into_engine();
        let grammars = engine.seen_grammars.lock().unwrap();
        let grammar = grammars[0].as_deref().expect("грамматика ушла в движок");
        assert!(grammar.contains("root ::="), "{grammar}");
        assert!(grammar.contains("category"), "{grammar}");
    }

    #[test]
    fn grammar_not_sent_when_disabled_by_config() {
        let engine = FakeEngine {
            answer: Ok(r#"{"category": "card"}"#.to_string()),
            seen_prompts: Mutex::new(Vec::new()),
            seen_grammars: Mutex::new(Vec::new()),
        };
        let provider = provider_with_grammar(engine, false);
        provider
            .complete(CompletionRequest {
                system_context: String::new(),
                prompt: "x".into(),
                contract_name: Some("C".into()),
                expects_structured_output: true,
                json_schema: Some(serde_json::json!({
                    "type": "object",
                    "properties": {"category": {"type": "string"}},
                    "required": ["category"]
                })),
            })
            .unwrap();
        let engine = provider.into_engine();
        assert!(engine.seen_grammars.lock().unwrap()[0].is_none());
    }

    #[test]
    fn unsupported_schema_falls_back_to_free_generation() {
        // anyOf — вне подмножества конвертера: предупреждение + None.
        let engine = FakeEngine {
            answer: Ok(r#"{"a": "x"}"#.to_string()),
            seen_prompts: Mutex::new(Vec::new()),
            seen_grammars: Mutex::new(Vec::new()),
        };
        let provider = provider_with_grammar(engine, true);
        provider
            .complete(CompletionRequest {
                system_context: String::new(),
                prompt: "x".into(),
                contract_name: Some("C".into()),
                expects_structured_output: true,
                json_schema: Some(serde_json::json!({"anyOf": [{"type": "string"}]})),
            })
            .unwrap();
        let engine = provider.into_engine();
        assert!(engine.seen_grammars.lock().unwrap()[0].is_none());
    }

    #[test]
    fn completion_wraps_engine_output_with_identity() {
        let provider = provider(FakeEngine {
            answer: Ok(r#"{"category": "card"}"#.to_string()),
            seen_prompts: Mutex::new(Vec::new()),
            seen_grammars: Mutex::new(Vec::new()),
        });
        let response = provider
            .complete(CompletionRequest {
                system_context: "правила".to_string(),
                prompt: "классифицируй".to_string(),
                contract_name: Some("ClassificationOut".to_string()),
                expects_structured_output: true,
                json_schema: None,
            })
            .unwrap();

        assert_eq!(response.raw_text, r#"{"category": "card"}"#);
        assert_eq!(response.model.model_id, "qwen3-4b-q4");
        assert_eq!(response.model.tier, ModelTier::Weak);
    }

    #[test]
    fn prompt_carries_system_context_and_contract_instruction() {
        let provider = provider(FakeEngine {
            answer: Ok("{}".to_string()),
            seen_prompts: Mutex::new(Vec::new()),
            seen_grammars: Mutex::new(Vec::new()),
        });
        let _ = provider.complete(CompletionRequest {
            system_context: "СИСТЕМА-МАРКЕР".to_string(),
            prompt: "ЗАДАЧА-МАРКЕР".to_string(),
            contract_name: Some("ClassificationOut".to_string()),
            expects_structured_output: true,
            json_schema: None,
        });

        let seen = provider.engine.seen_prompts.lock().unwrap();
        assert_eq!(seen.len(), 1);
        let (system, user, expects_json) = &seen[0];
        assert_eq!(system, "СИСТЕМА-МАРКЕР");
        assert!(user.contains("ЗАДАЧА-МАРКЕР"), "{user}");
        // Аналог response_format у HTTP: инструкция JSON в подсказке +
        // флаг для prefill у движка.
        assert!(user.contains("ClassificationOut"), "{user}");
        assert!(user.contains("JSON"), "{user}");
        assert!(expects_json);
    }

    #[test]
    fn engine_error_maps_to_model_error() {
        let provider = provider(FakeEngine {
            answer: Err("веса не читаются".to_string()),
            seen_prompts: Mutex::new(Vec::new()),
            seen_grammars: Mutex::new(Vec::new()),
        });
        let err = provider
            .complete(CompletionRequest {
                system_context: String::new(),
                prompt: "x".to_string(),
                contract_name: None,
                expects_structured_output: false,
                json_schema: None,
            })
            .expect_err("ошибка движка обязана доходить до вызывающего");
        assert!(err.to_string().contains("веса не читаются"), "{err}");
    }

    /// Без системного контекста подсказка не несёт пустого system-блока —
    /// слабой локальной модели лишний шум вреден (§3.5).
    #[test]
    fn empty_system_context_produces_no_system_block() {
        let provider = provider(FakeEngine {
            answer: Ok("ok".to_string()),
            seen_prompts: Mutex::new(Vec::new()),
            seen_grammars: Mutex::new(Vec::new()),
        });
        let _ = provider.complete(CompletionRequest {
            system_context: String::new(),
            prompt: "вопрос".to_string(),
            contract_name: None,
            expects_structured_output: false,
            json_schema: None,
        });
        let seen = provider.engine.seen_prompts.lock().unwrap();
        let (system, user, expects_json) = &seen[0];
        assert!(system.is_empty());
        assert_eq!(user, "вопрос");
        assert!(!expects_json);
    }
}
