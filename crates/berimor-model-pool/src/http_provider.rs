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
    model::{CompletionRequest, CompletionResponse, ModelError, ModelIdentity},
};
use std::time::Duration;

/// Техдолг TD3.4 (`docs/audit-2026-07-31.md`): не документированный
/// нигде в системе SLA, разумный дефолт — «есть хоть какой-то потолок»
/// важнее точной цифры. Клиент блокирующий (`reqwest::blocking`),
/// синхронный цикл `berimor run`: без таймаута зависший endpoint
/// блокировал бы весь процесс навсегда.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

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
    client: reqwest::blocking::Client,
}

impl OpenAiCompatibleProvider {
    /// `api_key` — `None` для endpoint'ов без аутентификации (локальные
    /// серверы). Ключ оборачивается в [`Secret`] на границе конфигурации.
    pub fn new(
        identity: ModelIdentity,
        base_url: String,
        api_key: Option<Secret>,
        allow_private_endpoint: bool,
    ) -> Result<Self, ModelError> {
        let client = reqwest::blocking::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|err| {
                ModelError::Unavailable(format!("не удалось собрать HTTP-клиент: {err}"))
            })?;
        Ok(Self {
            identity,
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key,
            allow_private_endpoint,
            client,
        })
    }

    pub fn identity(&self) -> &ModelIdentity {
        &self.identity
    }

    /// Сетевой гейт: извлекает хост из `base_url` и проверяет его.
    /// Приватный endpoint без явного opt-in — ошибка ДО любого соединения.
    fn check_network_gate(&self) -> Result<(), ModelError> {
        let host_port = self
            .base_url
            .split("://")
            .nth(1)
            .unwrap_or(&self.base_url)
            .split('/')
            .next()
            .unwrap_or_default();
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
    response_format: Option<ResponseFormat>,
}

#[derive(serde::Serialize)]
struct ChatMessage<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(serde::Serialize)]
struct ResponseFormat {
    #[serde(rename = "type")]
    kind: &'static str,
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
            temperature: 0.0,
            // Подсказка формата — только подсказка; валидирует ответ всё
            // равно Mediation, не сервер и не этот клиент. TD3.3: раньше
            // включалось по `contract_name.is_some()` — но CodeAct тоже
            // всегда передаёт `contract_name` (контракт результата, не
            // формат самого ответа модели), из-за чего структурно не мог
            // работать через реальный OpenAI-совместимый endpoint (сервер
            // заставлял бы модель ответить JSON вместо JS-текста).
            // Явное поле `expects_structured_output` — не вывод из
            // `contract_name`.
            response_format: request.expects_structured_output.then_some(ResponseFormat {
                kind: "json_object",
            }),
        };

        let mut http = self
            .client
            .post(format!("{}/chat/completions", self.base_url))
            .json(&body);
        if let Some(key) = &self.api_key {
            // Единственное место, где ключ покидает обёртку (F4).
            http = http.bearer_auth(key.reveal());
        }

        let response = http
            .send()
            .map_err(|err| ModelError::Unavailable(format!("HTTP-вызов провайдера: {err}")))?;
        let status = response.status();
        // Size-cap на тело ответа (аудит 3.9): читается не более
        // MAX+1 байт — сломанный/злонамеренный endpoint не может
        // исчерпать память клиента бесконечным телом.
        use std::io::Read as _;
        let mut body_bytes = Vec::new();
        response
            .take(MAX_RESPONSE_BODY_BYTES + 1)
            .read_to_end(&mut body_bytes)
            .map_err(|err| ModelError::Unavailable(format!("чтение ответа провайдера: {err}")))?;
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

        Ok(CompletionResponse {
            raw_text,
            model: self.identity.clone(),
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

    fn identity() -> ModelIdentity {
        ModelIdentity {
            provider: "mock".into(),
            model_id: "mock-model".into(),
            tier: ModelTier::Weak,
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

    /// Техдолг TD3.3: `contract_name` присутствует (нужен Mediation
    /// результата — CodeAct тоже его передаёт), но `expects_structured_output:
    /// false` — сервер не должен получить `response_format`. Раньше поле
    /// включалось по одному `contract_name.is_some()`, что делало CodeAct
    /// структурно неработоспособным через реальный endpoint.
    #[test]
    fn expects_structured_output_false_omits_response_format_even_with_a_contract_name() {
        let (url, server) = serve_once("200 OK", GOLDEN_RESPONSE.to_string());
        let provider = OpenAiCompatibleProvider::new(identity(), url, None, true).unwrap();
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
        let provider = OpenAiCompatibleProvider::new(identity(), url, None, true).unwrap();

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
        let provider = OpenAiCompatibleProvider::new(identity(), url, None, true).unwrap();
        assert!(provider.complete(request()).is_ok());
        server.join().unwrap();
    }

    #[test]
    fn http_error_maps_to_unavailable() {
        let (url, server) = serve_once("500 Internal Server Error", "{\"error\": \"boom\"}".into());
        let provider = OpenAiCompatibleProvider::new(identity(), url, None, true).unwrap();

        let result = provider.complete(request());

        assert!(matches!(result, Err(ModelError::Unavailable(_))));
        server.join().unwrap();
    }

    #[test]
    fn private_endpoint_without_optin_is_blocked_before_any_connection() {
        // Порт 9 (discard) закрыт наверняка: если ошибка — про соединение,
        // значит гейт пропустил запрос наружу, чего быть не должно.
        let provider =
            OpenAiCompatibleProvider::new(identity(), "http://127.0.0.1:9".into(), None, false)
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
        let provider =
            OpenAiCompatibleProvider::new(identity(), "http://203.0.113.10:9".into(), None, false)
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
