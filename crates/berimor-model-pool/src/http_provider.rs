//! HTTP-клиент удалённых провайдеров за общим интерфейсом Model Pool.
//!
//! Источник: `docs/arch/stack.md` §5 («тонкий типизированный HTTP-клиент
//! на провайдера за общим интерфейсом — без фреймворка оркестрации»).
//! ROADMAP: E5.
//!
//! Тонкость — принципиальна: клиент переводит [`CompletionRequest`] в
//! OpenAI-совместимый вызов и обратно, и ничего больше. Маршрутизация по
//! классам — `ModelPool` (E3), сборка подсказки — `StructuredLLM` (E2),
//! валидация ответа — Mediation. Здесь нет ни одного из этих решений.
//!
//! Две границы доверия, обе из security-model.md:
//! - сетевой гейт (S3): перед каждым запросом проверяется адрес endpoint'а;
//!   приватный адрес проходит только при явном opt-in владельца
//!   (`allow_private_endpoint` в конфигурации — это и есть форма
//!   подтверждения для неинтерактивного клиента, как
//!   `confirmation_mode = "off"` — форма подтверждения для профиля);
//! - секрет (F4): API-ключ хранится в [`Secret`], раскрывается ровно один
//!   раз — в заголовке запроса; в ошибках и логах не появляется никогда.

use berimor_capability::net_gate::{self, NetworkDecision};
use berimor_secrets::Secret;
use berimor_types::{
    executor::ModelProvider,
    model::{CompletionRequest, CompletionResponse, ModelError, ModelIdentity, ResponseFormat},
};
use std::time::Duration;

/// Диалект OpenAI-совместимого endpoint'а на проводе (SGR-волна 0.30.0,
/// issue #3, спека `docs/rnd/sgr-wave-spec.md` п.B4). Ollama принимает
/// схему в поле `format` (объект схемы / строка `"json"`), эталонный
/// OpenAI-диалект — в `response_format`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderDialect {
    OpenAi,
    Ollama,
}

impl ProviderDialect {
    /// Отдельного «ollama-провайдера» в дереве нет — ollama ходит через
    /// тот же OpenAI-совместимый `/v1` (пресет `ollama`, порт 11434),
    /// поэтому диалект определяется по конфигурации провайдера: имя
    /// пресета (`ollama`) или характерный порт по умолчанию. Переименованный
    /// провайдер на нестандартном порту получает OpenAi-диалект — ollama
    /// принимает и `response_format: json_schema` (structured outputs с
    /// 0.5), деградации нет.
    pub fn detect(provider_name: &str, base_url: &str) -> Self {
        if provider_name == "ollama" || base_url.contains(":11434") {
            Self::Ollama
        } else {
            Self::OpenAi
        }
    }
}

/// Политика подсказки формата ответа (спека п.B1/B3): режим из
/// конфигурации и диалект провода. Заменяет булев квирк
/// `json_object_response_format` (вывод из него — в
/// `ProviderConfig::effective_response_format`).
///
/// Это ПОДСКАЗКА транспорту, не гарантия: валидирует ответ всё равно
/// Mediation (M2/M3), а не сервер и не клиент.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FormatPolicy {
    pub response_format: ResponseFormat,
    pub dialect: ProviderDialect,
}

/// Техдолг TD3.4 (`docs/audit-2026-07-31.md`): не документированный
/// нигде в системе SLA, разумный дефолт — «есть хоть какой-то потолок»
/// важнее точной цифры. Клиент блокирующий (`reqwest::blocking`),
/// синхронный цикл `berimor run`: без таймаута зависший endpoint
/// блокировал бы весь процесс навсегда.
///
/// Директива 2026-08-08: было 30с — локальные reasoning-модели (LM
/// Studio) на первом же ходе агентного цикла (самый крупный системный
/// промпт — каталог инструментов) легко превышают это на decode-
/// скорости порядка 170 ток/с, ловя транспортный таймаут, который ещё
/// и ретраится 4 раза (см. `TRANSPORT_BACKOFF_MS`) — пользователь ждёт
/// ~2 минуты ради гарантированного отказа. Дефолт поднят ×5; для
/// провайдеров, которым и этого мало (или наоборот — нужен короткий
/// потолок для быстрого fail-over в облаке), есть
/// `ProviderConfig::request_timeout_secs`.
pub const DEFAULT_REQUEST_TIMEOUT_SECS: u64 = 150;

/// Верхний лимит тела HTTP-ответа провайдера (аудит 3.9). Реальный
/// ответ chat/completions — килобайты; лимит с большим запасом, но
/// конечный.
pub const MAX_RESPONSE_BODY_BYTES: u64 = 8 * 1024 * 1024;

/// Провайдер с OpenAI-совместимым HTTP API (`POST {base_url}/chat/completions`).
/// Такой формат де-факто поддерживает большинство удалённых провайдеров и
/// локальных серверов инференса — один клиент покрывает весь класс.
pub struct OpenAiCompatibleProvider {
    identity: ModelIdentity,
    base_url: String,
    api_key: Option<Secret>,
    allow_private_endpoint: bool,
    /// Явная температура из конфига; None — 0.0 (воспроизводимость).
    /// Часть моделей принимает только temperature=1 (Kimi k3: «only 1
    /// is allowed», репорт 2026-08-03) — для них пресет/конфиг задаёт
    /// её явно.
    temperature: Option<f32>,
    /// Политика формата ответа (SGR 0.30.0, issue #3). `ResponseFormat::
    /// None` — поле не отправляется вовсе (репорт 2026-08-08: LM Studio —
    /// 400 «response_format.type must be 'json_schema' or 'text'», живой
    /// прогон против localhost:1234 подтвердил и ошибку, и что без поля
    /// тот же запрос проходит 200 OK) — как уже происходит для CodeAct
    /// при `expects_structured_output: false`.
    format: FormatPolicy,
    client: reqwest::blocking::Client,
}

impl OpenAiCompatibleProvider {
    /// `api_key` — `None` для endpoint'ов без аутентификации (локальные
    /// серверы). Ключ оборачивается в [`Secret`] на границе конфигурации.
    /// `request_timeout_secs` — `None` берёт `DEFAULT_REQUEST_TIMEOUT_SECS`
    /// (директива 2026-08-08: локальным reasoning-моделям дефолта может
    /// не хватать — настраивается per-провайдер, не глобальной правкой
    /// константы).
    pub fn new(
        identity: ModelIdentity,
        base_url: String,
        api_key: Option<Secret>,
        allow_private_endpoint: bool,
        temperature: Option<f32>,
        format: FormatPolicy,
        request_timeout_secs: Option<u64>,
    ) -> Result<Self, ModelError> {
        // Находка 2.14 аудита: гейт применялся только к первому хопу —
        // reqwest следовал за 302 на непроверенный хост. Редирект для
        // LLM API — аномалия: fail-closed (3xx всплывёт как HTTP-ошибка),
        // как у встроенного http.fetch («редиректы не следуются»).
        let timeout =
            Duration::from_secs(request_timeout_secs.unwrap_or(DEFAULT_REQUEST_TIMEOUT_SECS));
        let client = reqwest::blocking::Client::builder()
            .timeout(timeout)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|err| {
                ModelError::Unavailable(format!("не удалось собрать HTTP-клиент: {err}"))
            })?;
        Ok(Self {
            identity,
            base_url: base_url.trim_end_matches('/').to_string(),
            temperature,
            api_key,
            allow_private_endpoint,
            format,
            client,
        })
    }

    pub fn identity(&self) -> &ModelIdentity {
        &self.identity
    }

    /// Сетевой гейт: извлекает хост из `base_url` и проверяет его.
    /// Приватный endpoint без явного opt-in — ошибка ДО любого соединения.
    fn check_network_gate(&self) -> Result<(), ModelError> {
        // Находка 3.11 аудита: userinfo (`http://user:pass@host/`)
        // сдвигал разбор — гейт проверял "user", соединение шло на host
        // (класс SSRF-обхода: гейт и соединение видят РАЗНЫЕ хосты).
        // Userinfo отрезается по ПОСЛЕДНЕМу '@' до разбора порта.
        let host_port = self
            .base_url
            .split("://")
            .nth(1)
            .unwrap_or(&self.base_url)
            .split('/')
            .next()
            .unwrap_or_default();
        let host_port = host_port.rsplit('@').next().unwrap_or(host_port);
        let (host, port) = match host_port.rsplit_once(':') {
            Some((h, p)) => (
                h.trim_start_matches('[').trim_end_matches(']'),
                p.parse().unwrap_or(443),
            ),
            None => (host_port, 443),
        };
        match net_gate::check_host(host, port) {
            NetworkDecision::Allow => Ok(()),
            NetworkDecision::ConfirmRequired { reason } if self.allow_private_endpoint => {
                let _ = reason; // opt-in владельца зафиксирован в конфигурации
                Ok(())
            }
            NetworkDecision::ConfirmRequired { reason } => Err(ModelError::Unavailable(format!(
                "сетевой гейт: {reason} (или задайте allow_private_endpoint для доверенного локального endpoint'а)"
            ))),
        }
    }
}

#[derive(serde::Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: [ChatMessage<'a>; 2],
    temperature: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_format: Option<WireResponseFormat<'a>>,
    /// Ollama-диалект (спека п.B4): поле `format` — строка "json"
    /// (json_object) или объект схемы (json_schema/grammar).
    #[serde(skip_serializing_if = "Option::is_none")]
    format: Option<serde_json::Value>,
}

#[derive(serde::Serialize)]
struct ChatMessage<'a> {
    role: &'a str,
    content: &'a str,
}

/// OpenAI-диалект на проводе: `response_format` с опциональным
/// вложенным `json_schema` (strict constrained decoding, issue #3).
#[derive(serde::Serialize)]
struct WireResponseFormat<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    json_schema: Option<WireJsonSchema<'a>>,
}

#[derive(serde::Serialize)]
struct WireJsonSchema<'a> {
    name: &'a str,
    schema: &'a serde_json::Value,
    strict: bool,
}

impl ModelProvider for OpenAiCompatibleProvider {
    fn complete(&self, request: CompletionRequest) -> Result<CompletionResponse, ModelError> {
        self.check_network_gate()?;

        let body = ChatRequest {
            model: &self.identity.model_id,
            messages: [
                ChatMessage {
                    role: "system",
                    content: &request.system_context,
                },
                ChatMessage {
                    role: "user",
                    content: &request.prompt,
                },
            ],
            // Структурированный шаг — не творческий: минимальная температура
            // ради воспроизводимости (replay по журналу, ideal-agent §3.11).
            // Конфиг провайдера переопределяет (Kimi k3 допускает только 1).
            temperature: self.temperature.unwrap_or(0.0),
            // Подсказка формата — только подсказка; валидирует ответ всё
            // равно Mediation, не сервер и не этот клиент. TD3.3: раньше
            // включалось по `contract_name.is_some()` — но CodeAct тоже
            // всегда передаёт `contract_name` (контракт результата, не
            // формат самого ответа модели), из-за чего структурно не мог
            // работать через реальный OpenAI-совместимый endpoint (сервер
            // заставлял бы модель ответить JSON вместо JS-текста).
            // Явное поле `expects_structured_output` — не вывод из
            // `contract_name`. SGR 0.30.0 (issue #3): режим и диалект —
            // из FormatPolicy провайдера.
            response_format: if !request.expects_structured_output {
                None
            } else {
                match (self.format.response_format, self.format.dialect) {
                    (ResponseFormat::None, _) => None,
                    (ResponseFormat::JsonObject, ProviderDialect::OpenAi) => {
                        Some(WireResponseFormat {
                            kind: "json_object",
                            json_schema: None,
                        })
                    }
                    (
                        ResponseFormat::JsonSchema | ResponseFormat::Grammar,
                        ProviderDialect::OpenAi,
                    ) => {
                        match (
                            request.json_schema.as_ref(),
                            request.contract_name.as_deref(),
                        ) {
                            (Some(schema), Some(name)) => Some(WireResponseFormat {
                                kind: "json_schema",
                                json_schema: Some(WireJsonSchema {
                                    name,
                                    schema,
                                    strict: true,
                                }),
                            }),
                            // Схемы у вызывающего нет — предупреждение и
                            // даунгрейд (спека п.B3): constrained decoding
                            // невозможен без схемы, молчать нельзя.
                            _ => {
                                eprintln!(
                                    "[berimor] response_format=json_schema, но вызывающий не передал схему — даунгрейд до json_object"
                                );
                                Some(WireResponseFormat {
                                    kind: "json_object",
                                    json_schema: None,
                                })
                            }
                        }
                    }
                    // Ollama-диалект — поле `format` ниже.
                    (_, ProviderDialect::Ollama) => None,
                }
            },
            format: if !request.expects_structured_output
                || self.format.dialect != ProviderDialect::Ollama
            {
                None
            } else {
                match self.format.response_format {
                    ResponseFormat::None => None,
                    ResponseFormat::JsonObject => Some(serde_json::Value::String("json".into())),
                    ResponseFormat::JsonSchema | ResponseFormat::Grammar => {
                        request.json_schema.clone().or_else(|| {
                            eprintln!(
                                "[berimor] ollama format=schema, но вызывающий не передал схему — даунгрейд до \"json\""
                            );
                            Some(serde_json::Value::String("json".into()))
                        })
                    }
                }
            },
        };

        // Ретраи на ТРАНСПОРТНЫЕ сбои (обрыв соединения, усечённое тело,
        // «error sending request» — директива 2026-08-03: «попробовать
        // повторный вызов через несколько секунд»): 4 попытки с backoff
        // 0.5/1.5/3с, затем ошибка наверх — там failover на следующего
        // провайдера того же класса (FailoverProvider). Логические
        // ошибки (4xx/5xx, не-JSON тела) не ретраятся.
        const TRANSPORT_BACKOFF_MS: [u64; 3] = [500, 1500, 3000];
        let mut last_transport_err: Option<ModelError> = None;
        let mut body_bytes = Vec::new();
        let mut status = reqwest::StatusCode::OK;
        for attempt in 0..=TRANSPORT_BACKOFF_MS.len() as u8 {
            let mut http = self
                .client
                .post(format!("{}/chat/completions", self.base_url))
                .json(&body);
            if let Some(key) = &self.api_key {
                // Единственное место, где ключ покидает обёртку (F4).
                http = http.bearer_auth(key.reveal());
            }
            let attempt_result = (|| {
                let response = http.send().map_err(|err| {
                    ModelError::Unavailable(format!("HTTP-вызов провайдера: {err}"))
                })?;
                status = response.status();
                // Size-cap на тело ответа (аудит 3.9): читается не более
                // MAX+1 байт — сломанный/злонамеренный endpoint не может
                // исчерпать память клиента бесконечным телом.
                use std::io::Read as _;
                body_bytes.clear();
                response
                    .take(MAX_RESPONSE_BODY_BYTES + 1)
                    .read_to_end(&mut body_bytes)
                    .map_err(|err| {
                        ModelError::Unavailable(format!("чтение ответа провайдера: {err}"))
                    })
            })();
            match attempt_result {
                Ok(_) => {
                    last_transport_err = None;
                    break;
                }
                Err(err) => {
                    last_transport_err = Some(err);
                    if let Some(pause) = TRANSPORT_BACKOFF_MS.get(attempt as usize) {
                        std::thread::sleep(std::time::Duration::from_millis(*pause));
                    }
                }
            }
        }
        if let Some(err) = last_transport_err {
            return Err(err);
        }
        if body_bytes.len() as u64 > MAX_RESPONSE_BODY_BYTES {
            return Err(ModelError::Unavailable(format!(
                "ответ провайдера превышает лимит тела ({MAX_RESPONSE_BODY_BYTES} байт)"
            )));
        }
        let body: serde_json::Value = serde_json::from_slice(&body_bytes)
            .map_err(|err| ModelError::Unavailable(format!("ответ провайдера не JSON: {err}")))?;
        if !status.is_success() {
            return Err(ModelError::Unavailable(format!(
                "провайдер ответил {status}: {}",
                body.get("error").unwrap_or(&body)
            )));
        }

        let raw_text = body["choices"][0]["message"]["content"]
            .as_str()
            .ok_or_else(|| {
                ModelError::Unavailable(format!(
                    "ответ провайдера без choices[0].message.content: {body}"
                ))
            })?
            .to_string();

        // Usage (волна A, 0.38.0): OpenAI-форма {prompt_tokens,
        // completion_tokens}; отсутствие — None, не ошибка.
        let usage = body.get("usage").and_then(|u| {
            Some(berimor_types::model::TokenUsage {
                prompt_tokens: u.get("prompt_tokens")?.as_u64()?,
                completion_tokens: u.get("completion_tokens")?.as_u64()?,
            })
        });

        Ok(CompletionResponse {
            raw_text,
            model: self.identity.clone(),
            usage,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use berimor_types::model::ModelTier;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;

    const GOLDEN_RESPONSE: &str =
        include_str!("../../../fixtures/golden/providers/chat-completion-response.json");

    /// Находка 3.11 аудита: userinfo не прячет приватный хост от гейта.
    #[test]
    fn userinfo_does_not_hide_private_host_from_gate() {
        let result = OpenAiCompatibleProvider::new(
            identity(),
            "http://user:pass@127.0.0.1:9".into(),
            None,
            false, // приватный endpoint БЕЗ opt-in — гейт обязан отказать
            None,
            policy_json_object(),
            None,
        )
        .and_then(|p| p.complete(request()));
        assert!(result.is_err(), "гейт обязан видеть 127.0.0.1 за userinfo");
    }

    /// Находка 2.14 аудита: редирект на второй хост НЕ следуется —
    /// 302 всплывает как ошибка (гейт не обходится через Location).
    #[test]
    fn redirect_to_other_host_is_not_followed() {
        // Сервер-редиректор: 302 + Location на «другой» endpoint.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let redirect_target = "http://127.0.0.1:1/never-checked";
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut head = [0u8; 2048];
            let _ = stream.read(&mut head);
            let response = format!(
                "HTTP/1.1 302 Found\r\nLocation: {redirect_target}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
            stream.write_all(response.as_bytes()).unwrap();
        });

        let provider = OpenAiCompatibleProvider::new(
            identity(),
            url,
            None,
            true,
            None,
            policy_json_object(),
            None,
        )
        .unwrap();
        let result = provider.complete(request());
        server.join().unwrap();
        assert!(
            result.is_err(),
            "302 обязан всплывать ошибкой, не следованием: {result:?}"
        );
    }

    fn identity() -> ModelIdentity {
        ModelIdentity {
            provider: "mock".into(),
            model_id: "mock-model".into(),
            tier: ModelTier::Weak,
        }
    }

    fn policy_json_object() -> FormatPolicy {
        FormatPolicy {
            response_format: ResponseFormat::JsonObject,
            dialect: ProviderDialect::OpenAi,
        }
    }

    fn policy_none() -> FormatPolicy {
        FormatPolicy {
            response_format: ResponseFormat::None,
            dialect: ProviderDialect::OpenAi,
        }
    }

    /// Минимальный HTTP-сервер на std: читает запрос, отвечает заданным
    /// телом. Возвращает (url, join handle). Достаточно для проверки
    /// клиента — e2e CLI4 использует свой, полноценнее.
    fn serve_once(status: &str, body: String) -> (String, std::thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let status = status.to_string();
        let handle = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream);
            let mut request_head = String::new();
            let mut content_length = 0usize;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line.to_lowercase().starts_with("content-length:") {
                    content_length = line[15..].trim().parse().unwrap();
                }
                request_head.push_str(&line);
                if line.trim().is_empty() {
                    break;
                }
            }
            let mut request_body = vec![0u8; content_length];
            reader.read_exact(&mut request_body).unwrap();
            let response = format!(
                "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            reader.get_mut().write_all(response.as_bytes()).unwrap();
            request_head + &String::from_utf8_lossy(&request_body)
        });
        (url, handle)
    }

    fn request() -> CompletionRequest {
        CompletionRequest {
            system_context: "Ты — классификатор.".into(),
            prompt: "Классифицируй обращение.".into(),
            contract_name: Some("ClassificationOut".into()),
            expects_structured_output: true,
            step_id: None,
            json_schema: None,
        }
    }

    #[test]
    fn successful_completion_returns_content_from_golden_shape() {
        let (url, server) = serve_once("200 OK", GOLDEN_RESPONSE.to_string());
        let provider = OpenAiCompatibleProvider::new(
            identity(),
            url,
            Some(Secret::new("sk-test".into())),
            true,
            None,
            policy_json_object(),
            None,
        )
        .unwrap();

        let response = provider.complete(request()).unwrap();

        let captured = server.join().unwrap();
        // Имена HTTP-заголовков регистронезависимы (RFC 9110 §5.1): hyper
        // шлёт их в нижнем регистре — сравнивать надо без учёта регистра.
        let lowered = captured.to_lowercase();
        assert!(
            lowered.contains("authorization: bearer sk-test"),
            "ключ уходит в заголовок запроса: {captured}"
        );
        assert!(lowered.contains("\"model\":\"mock-model\""));
        assert!(lowered.contains("\"response_format\":{\"type\":\"json_object\"}"));
        let parsed: serde_json::Value = serde_json::from_str(&response.raw_text).unwrap();
        assert_eq!(parsed["category"], "card");
        assert_eq!(response.model.provider, "mock");
    }

    /// Репорт 2026-08-08: LM Studio отвечает 400 «response_format.type
    /// must be 'json_schema' or 'text'» на `{"type": "json_object"}`
    /// (подтверждено живым прогоном против localhost:1234) — при таком
    /// квирке провайдера поле не должно уходить вовсе, даже когда шаг
    /// САМ по себе ждёт структурный вывод (`expects_structured_output:
    /// true`). Без этого фикса каждый структурный ход `berimor chat`
    /// падал на первом же запросе к LM Studio.
    #[test]
    fn json_object_response_format_false_omits_the_field_even_when_structured_output_is_expected() {
        let (url, server) = serve_once("200 OK", GOLDEN_RESPONSE.to_string());
        let provider =
            OpenAiCompatibleProvider::new(identity(), url, None, true, None, policy_none(), None)
                .unwrap();

        provider.complete(request()).unwrap();

        let captured = server.join().unwrap();
        assert!(
            !captured.to_lowercase().contains("response_format"),
            "квирк провайдера обязан гасить поле: {captured}"
        );
    }

    /// Техдолг TD3.3: `contract_name` присутствует (нужен Mediation
    /// результата — CodeAct тоже его передаёт), но `expects_structured_output:
    /// false` — сервер не должен получить `response_format`. Раньше поле
    /// включалось по одному `contract_name.is_some()`, что делало CodeAct
    /// структурно неработоспособным через реальный endpoint.
    #[test]
    fn expects_structured_output_false_omits_response_format_even_with_a_contract_name() {
        let (url, server) = serve_once("200 OK", GOLDEN_RESPONSE.to_string());
        let provider = OpenAiCompatibleProvider::new(
            identity(),
            url,
            None,
            true,
            None,
            policy_json_object(),
            None,
        )
        .unwrap();
        let request = CompletionRequest {
            expects_structured_output: false,
            ..request()
        };

        provider.complete(request).unwrap();

        let captured = server.join().unwrap();
        assert!(
            !captured.to_lowercase().contains("response_format"),
            "CodeAct не должен получать response_format: {captured}"
        );
    }

    /// Регрессионный тест аудита 3.9: тело ответа сверх лимита —
    /// детерминированная ошибка, не исчерпание памяти.
    #[test]
    fn oversized_response_body_is_rejected_with_limit_error() {
        let huge = format!("{{\"pad\": \"{}\"}}", "x".repeat(9 * 1024 * 1024));
        let (url, server) = serve_once("200 OK", huge);
        let provider = OpenAiCompatibleProvider::new(
            identity(),
            url,
            None,
            true,
            None,
            policy_json_object(),
            None,
        )
        .unwrap();

        let result = provider.complete(request());

        match result {
            Err(ModelError::Unavailable(reason)) => {
                assert!(
                    reason.contains("лимит тела"),
                    "ожидалась ошибка лимита тела, получено: {reason}"
                );
            }
            other => panic!("тело сверх лимита обязано отклоняться: {other:?}"),
        }
        server.join().unwrap();
    }

    /// Контроль границы: ответ ровно в пределах лимита проходит.
    #[test]
    fn response_within_limit_is_accepted() {
        let (url, server) = serve_once("200 OK", GOLDEN_RESPONSE.to_string());
        let provider = OpenAiCompatibleProvider::new(
            identity(),
            url,
            None,
            true,
            None,
            policy_json_object(),
            None,
        )
        .unwrap();
        assert!(provider.complete(request()).is_ok());
        server.join().unwrap();
    }

    /// Директива 2026-08-08: дефолт таймаута поднят ×5 (30с → 150с) —
    /// зафиксировано значение, не только комментарий.
    #[test]
    fn default_request_timeout_is_five_times_the_old_thirty_seconds() {
        assert_eq!(DEFAULT_REQUEST_TIMEOUT_SECS, 150);
    }

    /// `request_timeout_secs` реально доходит до HTTP-клиента: сервер,
    /// который отвечает МЕДЛЕННЕЕ заданного потолка, обязан дать
    /// транспортную ошибку — не 150с дефолта (тест был бы недопустимо
    /// медленным), а свой короткий предел.
    #[test]
    fn custom_request_timeout_shorter_than_default_is_honored() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut head = [0u8; 2048];
            let _ = stream.read(&mut head);
            // Дольше клиентского таймаута (1с) — клиент обязан отвалиться
            // раньше, чем сервер вообще начнёт отвечать.
            std::thread::sleep(Duration::from_secs(3));
            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\n{}");
        });

        let provider = OpenAiCompatibleProvider::new(
            identity(),
            url,
            None,
            true,
            None,
            policy_json_object(),
            Some(1),
        )
        .unwrap();
        let result = provider.complete(request());

        assert!(
            result.is_err(),
            "запрос обязан упасть по короткому таймауту, не дождавшись сервера"
        );
        server.join().unwrap();
    }

    #[test]
    fn http_error_maps_to_unavailable() {
        let (url, server) = serve_once("500 Internal Server Error", "{\"error\": \"boom\"}".into());
        let provider = OpenAiCompatibleProvider::new(
            identity(),
            url,
            None,
            true,
            None,
            policy_json_object(),
            None,
        )
        .unwrap();

        let result = provider.complete(request());

        assert!(matches!(result, Err(ModelError::Unavailable(_))));
        server.join().unwrap();
    }

    #[test]
    fn private_endpoint_without_optin_is_blocked_before_any_connection() {
        // Порт 9 (discard) закрыт наверняка: если ошибка — про соединение,
        // значит гейт пропустил запрос наружу, чего быть не должно.
        let provider = OpenAiCompatibleProvider::new(
            identity(),
            "http://127.0.0.1:9".into(),
            None,
            false,
            None,
            policy_json_object(),
            None,
        )
        .unwrap();

        let result = provider.complete(request());

        match result {
            Err(ModelError::Unavailable(reason)) => {
                assert!(
                    reason.contains("сетевой гейт"),
                    "ожидалась ошибка гейта, получено: {reason}"
                );
            }
            other => panic!("приватный endpoint без opt-in обязан блокироваться: {other:?}"),
        }
    }

    #[test]
    fn public_host_literal_passes_gate() {
        // DNS-имя не используем (тест без сети): гейт проверяется на
        // литерале. Сам запрос не отправляется — проверяется только
        // отсутствие ошибки гейта; порт закрыт, ошибка будет про соединение.
        let provider = OpenAiCompatibleProvider::new(
            identity(),
            "http://203.0.113.10:9".into(),
            None,
            false,
            None,
            policy_json_object(),
            None,
        )
        .unwrap();

        let result = provider.complete(request());

        match result {
            Err(ModelError::Unavailable(reason)) => {
                assert!(
                    !reason.contains("сетевой гейт"),
                    "публичный адрес не должен блокироваться гейтом: {reason}"
                );
            }
            other => panic!("закрытый порт обязан дать ошибку соединения: {other:?}"),
        }
    }
}
