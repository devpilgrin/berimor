//! Инструмент `decision.ask` — провайдеры решений System One: Jev (облако
//! TypeSafe, `https://api.typesafe.ai`) и Kev (локальный `kev.serve`). Оба
//! говорят одним протоколом `POST /v1/systemone` (TypeSafe SDK 0.6.0,
//! `Authorization: Bearer <key>`), поэтому клиент один, а провайдеры — строки
//! конфига `[[decision_providers]]`.
//!
//! Провайдер решений НЕ генерирует текст: на вопросы Noul/Choice/Score он
//! возвращает калиброванные вероятности. Инструмент добавляет к каждому
//! ответу строку `routed` для `branch` (`yes`/`no`, ключ варианта, метка
//! уровня шкалы) и `uncertain`, если уверенность ниже `min_confidence`.
//! Маршрут по-прежнему выбирает код (I1), но теперь по значению с измеренной
//! уверенностью, а не по числу, которое модель написала текстом.
//!
//! Опциональность — три уровня:
//! - секции нет — инструмент не объявляется модели, вызов из процесса
//!   отвечает говорящей ошибкой (или деградирует, см. `on_failure`);
//! - `api_key_env` задан, переменная пуста — провайдер помечается
//!   недоступным при сборке, запуск НЕ падает;
//! - `on_failure: "uncertain"` — если ответить некому, все ответы
//!   `routed = "uncertain"` (`degraded: true`): процесс уходит в свою ветку
//!   для человека, а не падает. Так один процесс работает и без Jev/Kev.
//!
//! Откат: без явного `provider` провайдеры пробуются по порядку конфига. К
//! следующему переходим при сбое провайдера (сеть, таймаут, гейт, 401/403/
//! 404/408/429/5xx, нарушение протокола); НЕ переходим при 400/413/422 —
//! это ошибка самого запроса, другой провайдер её не исправит. Все
//! пропущенные — в `skipped` ответа: журнал видит, кто и почему не ответил.
//!
//! mutates: false (декларирует `builtin_policies`): инструмент ничего не
//! меняет в мире; отправка `state` провайдеру — то же доверие владельца,
//! что у `[[providers]]` (адрес задан в конфиге, не моделью).

use berimor_capability::net_gate::{self, NetworkDecision};
use berimor_executors::tool_only::{DispatchError, ToolDispatch};
use berimor_secrets::{Masker, Secret};
use serde_json::{json, Map, Value};
use std::io::Read;
use std::sync::Arc;
use std::time::Duration;

use crate::builtin_dispatch::err_str;
use crate::config::DecisionProviderConfig;

pub const TOOL: &str = "decision.ask";
const SYSTEM_ONE_PATH: &str = "/v1/systemone";
/// Kev-4B на CPU — ~2 с на вопрос; дефолт SDK (10 с) для него тесен.
const DEFAULT_TIMEOUT_SECS: u64 = 30;
const BODY_CAP: u64 = 1024 * 1024;
/// Серверный контекст Kev — 8192 токена на state; кап в байтах — защита
/// памяти, точный предел знает провайдер.
const STATE_CAP: usize = 256 * 1024;
const MAX_QUESTIONS: usize = 64;
/// Ответ провайдера «запрос неверен» — откат к другому бесполезен.
const REQUEST_ERROR_STATUSES: &[u16] = &[400, 413, 422];
const UNCERTAIN: &str = "uncertain";

pub struct DecisionProvider {
    name: String,
    endpoint: String,
    model: String,
    api_key: Option<Secret>,
    allow_private_endpoint: bool,
    /// Причина недоступности, выявленная при сборке (нет ключа): провайдер
    /// остаётся в списке, чтобы `skipped` объяснял, почему он молчит.
    unavailable: Option<String>,
    client: reqwest::blocking::Client,
}

struct AttemptError {
    reason: String,
    try_next: bool,
}

impl DecisionProvider {
    /// Ошибка — только конфигурационная (кривой URL), она фатальна на
    /// старте, как у прочих секций. Отсутствие ключа — не ошибка.
    pub fn from_config(cfg: &DecisionProviderConfig, masker: &mut Masker) -> Result<Self, String> {
        let base = cfg.base_url.trim_end_matches('/');
        if !(base.starts_with("http://") || base.starts_with("https://")) {
            return Err(format!(
                "decision_providers '{}': base_url должен начинаться с http:// или https://",
                cfg.name
            ));
        }
        let mut unavailable = None;
        let api_key = match &cfg.api_key_env {
            None => None,
            Some(env) => match std::env::var(env).ok().filter(|v| !v.is_empty()) {
                Some(value) => {
                    if value.len() >= berimor_secrets::MIN_SECRET_LEN {
                        masker.register(Secret::new(value.clone()));
                    } else {
                        eprintln!(
                            "[berimor] предупреждение: ключ провайдера решений '{}' короче {} символов — не регистрируется в маскировщике",
                            cfg.name,
                            berimor_secrets::MIN_SECRET_LEN
                        );
                    }
                    Some(Secret::new(value))
                }
                None => {
                    let reason = format!("переменная окружения {env} не задана");
                    eprintln!(
                        "[berimor] провайдер решений '{}' недоступен: {reason} — запуск продолжается без него",
                        cfg.name
                    );
                    unavailable = Some(reason);
                    None
                }
            },
        };
        let timeout = Duration::from_secs(cfg.request_timeout_secs.unwrap_or(DEFAULT_TIMEOUT_SECS));
        Ok(Self {
            name: cfg.name.clone(),
            endpoint: format!("{base}{SYSTEM_ONE_PATH}"),
            model: cfg.model.clone(),
            api_key,
            allow_private_endpoint: cfg.allow_private_endpoint,
            unavailable,
            client: build_client(timeout)
                .map_err(|e| format!("decision_providers '{}': {e}", cfg.name))?,
        })
    }

    fn ask(&self, state: &str, questions: &Value) -> Result<Value, AttemptError> {
        let fail = |reason: String, try_next: bool| AttemptError { reason, try_next };
        if let Some(reason) = &self.unavailable {
            return Err(fail(reason.clone(), true));
        }
        match net_gate::check_endpoint(&self.endpoint) {
            NetworkDecision::Allow => {}
            NetworkDecision::ConfirmRequired { .. } if self.allow_private_endpoint => {}
            NetworkDecision::ConfirmRequired { reason } => {
                return Err(fail(
                    format!("сетевой гейт: {reason} (для локального kev.serve задайте allow_private_endpoint = true)"),
                    true,
                ))
            }
        }
        let mut request = self
            .client
            .post(&self.endpoint)
            .json(&json!({"state": state, "model": self.model, "questions": questions}));
        if let Some(key) = &self.api_key {
            request = request.bearer_auth(key.reveal());
        }
        let response = request
            .send()
            .map_err(|e| fail(format!("запрос: {e}"), true))?;
        let status = response.status().as_u16();
        let mut buf = Vec::new();
        response
            .take(BODY_CAP + 1)
            .read_to_end(&mut buf)
            .map_err(|e| fail(format!("чтение ответа: {e}"), true))?;
        if buf.len() as u64 > BODY_CAP {
            return Err(fail("ответ больше 1 МиБ".into(), true));
        }
        if !(200..300).contains(&status) {
            let excerpt: String = String::from_utf8_lossy(&buf).chars().take(300).collect();
            return Err(fail(
                format!("статус {status}: {excerpt}"),
                !REQUEST_ERROR_STATUSES.contains(&status),
            ));
        }
        serde_json::from_slice(&buf).map_err(|e| fail(format!("ответ не JSON: {e}"), true))
    }
}

fn build_client(timeout: Duration) -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .timeout(timeout)
        // Цель редиректа не проходила бы гейт — как у http.fetch и LLM-провайдеров.
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(concat!("berimor/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| format!("http-клиент: {e}"))
}

struct Request<'a> {
    state: &'a str,
    questions: &'a Map<String, Value>,
    provider: Option<&'a str>,
    min_confidence: Option<f64>,
    degrade_on_failure: bool,
}

fn parse_args(args: &Value) -> Result<Request<'_>, DispatchError> {
    let state = args
        .get("state")
        .and_then(Value::as_str)
        .ok_or_else(|| err_str(TOOL, "аргумент 'state' обязателен (строка)"))?;
    if state.trim().is_empty() {
        return Err(err_str(TOOL, "аргумент 'state' пуст"));
    }
    if state.len() > STATE_CAP {
        return Err(err_str(TOOL, format!("'state' длиннее {STATE_CAP} байт")));
    }
    let questions = args
        .get("questions")
        .and_then(Value::as_object)
        .ok_or_else(|| err_str(TOOL, "аргумент 'questions' обязателен (объект id → вопрос)"))?;
    if questions.is_empty() || questions.len() > MAX_QUESTIONS {
        return Err(err_str(
            TOOL,
            format!("'questions': от 1 до {MAX_QUESTIONS} вопросов"),
        ));
    }
    for (id, question) in questions {
        validate_question(question)
            .map_err(|reason| err_str(TOOL, format!("вопрос '{id}': {reason}")))?;
    }
    let provider =
        match args.get("provider") {
            None | Some(Value::Null) => None,
            Some(value) => Some(value.as_str().ok_or_else(|| {
                err_str(TOOL, "'provider' — строка (имя из [[decision_providers]])")
            })?),
        };
    let min_confidence = match args.get("min_confidence") {
        None | Some(Value::Null) => None,
        Some(value) => Some(
            value
                .as_f64()
                .filter(|m| (0.0..=1.0).contains(m))
                .ok_or_else(|| err_str(TOOL, "'min_confidence' — число от 0 до 1"))?,
        ),
    };
    let degrade_on_failure = match args.get("on_failure").and_then(Value::as_str) {
        None | Some("error") => false,
        Some(UNCERTAIN) => true,
        Some(other) => {
            return Err(err_str(
                TOOL,
                format!("'on_failure': error | uncertain, получено '{other}'"),
            ))
        }
    };
    Ok(Request {
        state,
        questions,
        provider,
        min_confidence,
        degrade_on_failure,
    })
}

fn validate_question(question: &Value) -> Result<(), &'static str> {
    let has_instructions = question
        .get("instructions")
        .and_then(Value::as_str)
        .is_some_and(|s| !s.trim().is_empty());
    if !has_instructions {
        return Err("нужно непустое 'instructions'");
    }
    let criteria = question.get("criteria").filter(|c| !c.is_null());
    match question.get("type").and_then(Value::as_str) {
        Some("noul") if criteria.is_some_and(|c| !c.is_object()) => {
            Err("noul: 'criteria' — объект {true: …, false: …} или отсутствует")
        }
        Some("noul") => Ok(()),
        Some("choice") if criteria.and_then(Value::as_object).map_or(0, Map::len) < 2 => {
            Err("choice: 'criteria' — объект из ≥2 вариантов ключ → описание")
        }
        Some("score") if criteria.and_then(Value::as_array).map_or(0, Vec::len) < 2 => {
            Err("score: 'criteria' — массив из ≥2 уровней шкалы")
        }
        Some("choice" | "score") => Ok(()),
        _ => Err("'type' — noul | choice | score"),
    }
}

/// Ответы провайдера + `confidence` и `routed` на каждый. Нарушение
/// протокола — ошибка провайдера (откат к следующему), не молчаливый
/// `uncertain`: иначе сломанный провайдер выглядел бы «неуверенным».
fn route_answers(body: &Value, min_confidence: Option<f64>) -> Result<Map<String, Value>, String> {
    let answers = body
        .get("answers")
        .and_then(Value::as_object)
        .ok_or("в ответе нет объекта answers")?;
    let number = |answer: &Map<String, Value>, field: &str, id: &str| {
        answer
            .get(field)
            .and_then(Value::as_f64)
            .ok_or_else(|| format!("ответ '{id}': нет числового поля {field}"))
    };
    let mut routed_answers = Map::new();
    for (id, answer) in answers {
        let mut answer = answer
            .as_object()
            .cloned()
            .ok_or_else(|| format!("ответ '{id}' — не объект"))?;
        let (routed, confidence) = match answer.get("type").and_then(Value::as_str) {
            Some("noul") => {
                let p = number(&answer, "noul", id)?;
                (
                    if p >= 0.5 { "yes" } else { "no" }.to_string(),
                    p.max(1.0 - p),
                )
            }
            Some("choice") => {
                let choice = answer
                    .get("choice")
                    .and_then(Value::as_str)
                    .ok_or_else(|| format!("ответ '{id}': нет поля choice"))?
                    .to_string();
                (choice, number(&answer, "confidence", id)?)
            }
            Some("score") => {
                let top_level = answer
                    .get("probabilities")
                    .and_then(Value::as_object)
                    .and_then(|probs| {
                        probs
                            .iter()
                            .filter_map(|(level, p)| p.as_f64().map(|p| (level, p)))
                            .max_by(|a, b| a.1.total_cmp(&b.1))
                            .map(|(level, _)| level.clone())
                    })
                    .ok_or_else(|| format!("ответ '{id}': нет probabilities"))?;
                let label = answer
                    .get("legend")
                    .and_then(|legend| legend.get(&top_level))
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .unwrap_or(top_level);
                (label, number(&answer, "confidence", id)?)
            }
            other => return Err(format!("ответ '{id}': неизвестный тип {other:?}")),
        };
        let routed = match min_confidence {
            Some(min) if confidence < min => UNCERTAIN.to_string(),
            _ => routed,
        };
        answer.insert("confidence".into(), json!(confidence));
        answer.insert("routed".into(), json!(routed));
        routed_answers.insert(id.clone(), Value::Object(answer));
    }
    Ok(routed_answers)
}

fn degraded_answers(questions: &Map<String, Value>) -> Map<String, Value> {
    questions
        .iter()
        .map(|(id, question)| {
            (
                id.clone(),
                json!({"type": question["type"], "routed": UNCERTAIN, "confidence": 0.0}),
            )
        })
        .collect()
}

/// Обёртка-диспетчер (прецедент `MemoryToolDispatch`): `decision.ask`
/// исполняет сам, остальные инструменты делегирует внутренней цепочке.
/// Ставится всегда — пустой список провайдеров и есть режим «без них».
pub struct DecisionToolDispatch {
    providers: Vec<DecisionProvider>,
    inner: Arc<dyn ToolDispatch + Send + Sync>,
}

impl DecisionToolDispatch {
    pub fn new(
        providers: Vec<DecisionProvider>,
        inner: Arc<dyn ToolDispatch + Send + Sync>,
    ) -> Self {
        Self { providers, inner }
    }

    fn ask(&self, args: &Value) -> Result<Value, DispatchError> {
        let request = parse_args(args)?;
        let candidates: Vec<&DecisionProvider> = match request.provider {
            Some(name) => {
                vec![self
                    .providers
                    .iter()
                    .find(|p| p.name == name)
                    .ok_or_else(|| {
                        let known: Vec<&str> =
                            self.providers.iter().map(|p| p.name.as_str()).collect();
                        err_str(
                            TOOL,
                            format!(
                                "провайдер '{name}' не настроен; доступны: [{}]",
                                known.join(", ")
                            ),
                        )
                    })?]
            }
            None => self.providers.iter().collect(),
        };
        let questions = Value::Object(request.questions.clone());
        let mut skipped = Vec::new();
        for provider in candidates {
            let attempt = provider.ask(request.state, &questions).and_then(|body| {
                route_answers(&body, request.min_confidence)
                    .map(|answers| (body, answers))
                    .map_err(|reason| AttemptError {
                        reason: format!("протокол: {reason}"),
                        try_next: true,
                    })
            });
            match attempt {
                Ok((body, answers)) => {
                    return Ok(json!({
                        "provider": provider.name,
                        "model": provider.model,
                        "answers": answers,
                        "latency_ms": body.get("latency_ms").cloned().unwrap_or(Value::Null),
                        "skipped": skipped,
                        "degraded": false,
                    }))
                }
                Err(err) => {
                    skipped.push(json!({"provider": provider.name, "reason": err.reason}));
                    if !err.try_next {
                        // Запрос неверен сам по себе: деградация спрятала бы
                        // ошибку в процессе — отдаём её как есть.
                        return Err(err_str(
                            TOOL,
                            format!(
                                "провайдер '{}' отверг запрос: {}",
                                provider.name, err.reason
                            ),
                        ));
                    }
                }
            }
        }
        if request.degrade_on_failure {
            return Ok(json!({
                "provider": Value::Null,
                "model": Value::Null,
                "answers": degraded_answers(request.questions),
                "latency_ms": Value::Null,
                "skipped": skipped,
                "degraded": true,
            }));
        }
        if self.providers.is_empty() {
            return Err(err_str(
                TOOL,
                "провайдеры решений не настроены: добавьте [[decision_providers]] (Kev — локальный kev.serve, Jev — api.typesafe.ai; см. docs/decision-providers.md) или передайте on_failure: \"uncertain\"",
            ));
        }
        let reasons: Vec<String> = skipped
            .iter()
            .map(|s| {
                format!(
                    "{}: {}",
                    s["provider"].as_str().unwrap_or("?"),
                    s["reason"].as_str().unwrap_or("?")
                )
            })
            .collect();
        Err(err_str(
            TOOL,
            format!(
                "ни один провайдер решений не ответил — {}",
                reasons.join("; ")
            ),
        ))
    }
}

impl ToolDispatch for DecisionToolDispatch {
    fn call(&self, tool: &str, args: &Value) -> Result<Value, DispatchError> {
        if tool == TOOL {
            self.ask(args)
        } else {
            self.inner.call(tool, args)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::net::TcpListener;
    use std::sync::Mutex;

    struct EchoInner;
    impl ToolDispatch for EchoInner {
        fn call(&self, tool: &str, _args: &Value) -> Result<Value, DispatchError> {
            Ok(json!({"inner": tool}))
        }
    }

    fn provider(
        name: &str,
        base_url: &str,
        key: Option<&str>,
        allow_private: bool,
    ) -> DecisionProvider {
        DecisionProvider {
            name: name.into(),
            endpoint: format!("{base_url}{SYSTEM_ONE_PATH}"),
            model: "kev-latest".into(),
            api_key: key.map(|k| Secret::new(k.into())),
            allow_private_endpoint: allow_private,
            unavailable: None,
            client: build_client(Duration::from_secs(5)).unwrap(),
        }
    }

    fn dispatch(providers: Vec<DecisionProvider>) -> DecisionToolDispatch {
        DecisionToolDispatch::new(providers, Arc::new(EchoInner))
    }

    /// Мок kev.serve: одно соединение, запрос читается целиком (заголовки +
    /// тело по Content-Length), ответ с `Connection: close`.
    fn mock_server(status: u16, body: &str) -> (String, Arc<Mutex<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let captured = Arc::new(Mutex::new(String::new()));
        let captured_in_thread = Arc::clone(&captured);
        let body = body.to_string();
        std::thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut request = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                let n = stream.read(&mut chunk).unwrap_or(0);
                if n == 0 {
                    break;
                }
                request.extend_from_slice(&chunk[..n]);
                if let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&request[..end]).to_lowercase();
                    let length = head
                        .lines()
                        .find_map(|l| l.strip_prefix("content-length:"))
                        .and_then(|v| v.trim().parse::<usize>().ok())
                        .unwrap_or(0);
                    if request.len() >= end + 4 + length {
                        break;
                    }
                }
            }
            *captured_in_thread.lock().unwrap() = String::from_utf8_lossy(&request).into_owned();
            let response = format!(
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes());
        });
        (format!("http://{addr}"), captured)
    }

    fn kev_response() -> String {
        json!({
            "model": "kev-latest",
            "answers": {
                "billing": {"type": "noul", "noul": 0.2},
                "team": {"type": "choice", "choice": "billing", "confidence": 0.54,
                         "probabilities": {"returns": 0.29, "shipping": 0.01, "billing": 0.70}},
                "urgency": {"type": "score", "score": 1.6, "confidence": 0.78,
                            "legend": {"0": "низкая", "1": "средняя", "2": "высокая"},
                            "probabilities": {"0": 0.05, "1": 0.30, "2": 0.65}}
            },
            "latency_ms": 432.6
        })
        .to_string()
    }

    fn ask_args() -> Value {
        json!({
            "state": "С меня дважды списали деньги за подписку",
            "questions": {
                "billing": {"type": "noul", "instructions": "Это про оплату?"},
                "team": {"type": "choice", "instructions": "Какая команда?",
                         "criteria": {"returns": "возвраты", "shipping": "доставка", "billing": "оплата"}},
                "urgency": {"type": "score", "instructions": "Срочность?", "criteria": ["низкая", "средняя", "высокая"]}
            }
        })
    }

    #[test]
    fn other_tools_go_to_inner_dispatch() {
        let result = dispatch(vec![]).call("files.read", &json!({})).unwrap();
        assert_eq!(result["inner"], "files.read");
    }

    #[test]
    fn without_providers_error_explains_how_to_enable() {
        let err = dispatch(vec![]).call(TOOL, &ask_args()).unwrap_err();
        assert!(
            err.reason.contains("[[decision_providers]]"),
            "{}",
            err.reason
        );
    }

    #[test]
    fn without_providers_on_failure_uncertain_degrades_instead_of_failing() {
        let mut args = ask_args();
        args["on_failure"] = json!("uncertain");
        let result = dispatch(vec![]).call(TOOL, &args).unwrap();
        assert_eq!(result["degraded"], true);
        for id in ["billing", "team", "urgency"] {
            assert_eq!(result["answers"][id]["routed"], "uncertain", "{id}");
        }
        assert_eq!(result["answers"]["team"]["type"], "choice");
    }

    #[test]
    fn invalid_args_are_rejected_before_network() {
        // Порт 9 никто не слушает: дойди валидация до сети — ошибка была бы другой.
        let d = dispatch(vec![provider("kev", "http://127.0.0.1:9", None, true)]);
        let cases = [
            (json!({"questions": {}}), "'state'"),
            (
                json!({"state": "x", "questions": {"q": {"type": "maybe", "instructions": "?"}}}),
                "noul | choice | score",
            ),
            (
                json!({"state": "x", "questions": {"q": {"type": "choice", "instructions": "?", "criteria": {"a": "a"}}}}),
                "≥2 вариантов",
            ),
            (
                json!({"state": "x", "questions": {"q": {"type": "score", "instructions": "?"}}}),
                "≥2 уровней",
            ),
            (
                json!({"state": "x", "questions": {"q": {"type": "noul", "instructions": " "}}}),
                "instructions",
            ),
            (
                json!({"state": "x", "questions": {"q": {"type": "noul", "instructions": "?"}}, "min_confidence": 1.5}),
                "min_confidence",
            ),
            (
                json!({"state": "x", "questions": {"q": {"type": "noul", "instructions": "?"}}, "on_failure": "retry"}),
                "on_failure",
            ),
        ];
        for (args, expected) in cases {
            let err = d.call(TOOL, &args).unwrap_err();
            assert!(
                err.reason.contains(expected),
                "ожидалось «{expected}», получено: {}",
                err.reason
            );
        }
    }

    #[test]
    fn answers_get_routed_values_and_request_follows_protocol() {
        let (base, captured) = mock_server(200, &kev_response());
        let result = dispatch(vec![provider("kev", &base, None, true)])
            .call(TOOL, &ask_args())
            .unwrap();

        assert_eq!(result["provider"], "kev");
        assert_eq!(result["degraded"], false);
        let answers = &result["answers"];
        assert_eq!(answers["billing"]["routed"], "no");
        let noul_confidence = answers["billing"]["confidence"].as_f64().unwrap();
        assert!(
            (noul_confidence - 0.8).abs() < 1e-9,
            "max(p, 1-p): {noul_confidence}"
        );
        assert_eq!(answers["team"]["routed"], "billing");
        assert_eq!(answers["urgency"]["routed"], "высокая");
        assert_eq!(answers["urgency"]["score"], 1.6);

        let request = captured.lock().unwrap().clone();
        assert!(request.starts_with("POST /v1/systemone "), "{request}");
        assert!(
            !request.to_lowercase().contains("authorization:"),
            "без ключа — без заголовка: {request}"
        );
        let body: Value = serde_json::from_str(request.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!(body["model"], "kev-latest");
        assert_eq!(body["questions"]["team"]["criteria"]["billing"], "оплата");
    }

    #[test]
    fn low_confidence_is_routed_as_uncertain() {
        let (base, _) = mock_server(200, &kev_response());
        let mut args = ask_args();
        args["min_confidence"] = json!(0.6);
        let result = dispatch(vec![provider("kev", &base, None, true)])
            .call(TOOL, &args)
            .unwrap();
        let answers = &result["answers"];
        assert_eq!(
            answers["team"]["routed"], "uncertain",
            "confidence 0.54 < 0.6"
        );
        assert_eq!(
            answers["team"]["choice"], "billing",
            "исходный ответ сохраняется"
        );
        assert_eq!(answers["billing"]["routed"], "no", "confidence 0.8 >= 0.6");
        assert_eq!(
            answers["urgency"]["routed"], "высокая",
            "confidence 0.78 >= 0.6"
        );
    }

    #[test]
    fn bearer_key_is_sent_when_configured() {
        let (base, captured) = mock_server(200, &kev_response());
        dispatch(vec![provider(
            "jev",
            &base,
            Some("ts-test-key-123456"),
            true,
        )])
        .call(TOOL, &ask_args())
        .unwrap();
        let request = captured.lock().unwrap().to_lowercase();
        assert!(
            request.contains("authorization: bearer ts-test-key-123456"),
            "{request}"
        );
    }

    #[test]
    fn provider_failure_falls_back_to_next_and_is_reported() {
        let (down, _) = mock_server(503, "overloaded");
        let (up, _) = mock_server(200, &kev_response());
        let result = dispatch(vec![
            provider("jev", &down, None, true),
            provider("kev", &up, None, true),
        ])
        .call(TOOL, &ask_args())
        .unwrap();
        assert_eq!(result["provider"], "kev");
        assert_eq!(result["skipped"][0]["provider"], "jev");
        assert!(result["skipped"][0]["reason"]
            .as_str()
            .unwrap()
            .contains("503"));
    }

    #[test]
    fn rejected_request_does_not_fall_back() {
        let (bad, _) = mock_server(422, r#"{"detail":"criteria"}"#);
        // Второй провайдер не слушает: откат к нему дал бы другую ошибку.
        let err = dispatch(vec![
            provider("kev", &bad, None, true),
            provider("jev", "http://127.0.0.1:9", None, true),
        ])
        .call(TOOL, &ask_args())
        .unwrap_err();
        assert!(
            err.reason.contains("отверг запрос") && err.reason.contains("422"),
            "{}",
            err.reason
        );
    }

    #[test]
    fn private_endpoint_requires_owner_opt_in() {
        let (base, _) = mock_server(200, &kev_response());
        let err = dispatch(vec![provider("kev", &base, None, false)])
            .call(TOOL, &ask_args())
            .unwrap_err();
        assert!(
            err.reason.contains("allow_private_endpoint"),
            "{}",
            err.reason
        );
    }

    #[test]
    fn broken_protocol_is_a_provider_failure_not_uncertainty() {
        let (base, _) = mock_server(200, r#"{"answers": {"team": {"type": "choice"}}}"#);
        let err = dispatch(vec![provider("kev", &base, None, true)])
            .call(TOOL, &ask_args())
            .unwrap_err();
        assert!(err.reason.contains("протокол"), "{}", err.reason);
    }

    #[test]
    fn missing_key_marks_provider_unavailable_without_failing_startup() {
        let cfg = DecisionProviderConfig {
            name: "jev".into(),
            base_url: "https://api.typesafe.ai".into(),
            model: "jev-latest".into(),
            api_key_env: Some("BERIMOR_TEST_DECISION_KEY_NEVER_SET_7F3A".into()),
            allow_private_endpoint: false,
            request_timeout_secs: None,
        };
        let jev = DecisionProvider::from_config(&cfg, &mut Masker::new()).unwrap();
        let (up, _) = mock_server(200, &kev_response());
        let result = dispatch(vec![jev, provider("kev", &up, None, true)])
            .call(TOOL, &ask_args())
            .unwrap();
        assert_eq!(result["provider"], "kev");
        let reason = result["skipped"][0]["reason"].as_str().unwrap();
        assert!(
            reason.contains("BERIMOR_TEST_DECISION_KEY_NEVER_SET_7F3A"),
            "{reason}"
        );
    }

    #[test]
    fn non_http_base_url_is_a_config_error() {
        let cfg = DecisionProviderConfig {
            name: "kev".into(),
            base_url: "127.0.0.1:8009".into(),
            model: "kev-latest".into(),
            api_key_env: None,
            allow_private_endpoint: true,
            request_timeout_secs: None,
        };
        assert!(DecisionProvider::from_config(&cfg, &mut Masker::new()).is_err());
    }

    #[test]
    fn explicit_unknown_provider_lists_configured_ones() {
        let mut args = ask_args();
        args["provider"] = json!("gpt");
        let err = dispatch(vec![provider("kev", "http://127.0.0.1:9", None, true)])
            .call(TOOL, &args)
            .unwrap_err();
        assert!(
            err.reason.contains("'gpt'") && err.reason.contains("kev"),
            "{}",
            err.reason
        );
    }

    #[test]
    fn config_section_is_optional_and_merges_by_name() {
        let global: crate::config::PartialConfig = toml::from_str(
            r#"
[[decision_providers]]
name = "jev"
base_url = "https://api.typesafe.ai"
model = "jev-latest"
api_key_env = "TYPESAFE_API_KEY"

[[decision_providers]]
name = "kev"
base_url = "http://127.0.0.1:8009"
model = "kev-latest"
allow_private_endpoint = true
"#,
        )
        .unwrap();
        let local: crate::config::PartialConfig = toml::from_str(
            r#"
[[decision_providers]]
name = "kev"
base_url = "http://127.0.0.1:9009"
model = "kev-latest"
allow_private_endpoint = true
"#,
        )
        .unwrap();
        let merged = crate::config::merge(global, local);
        let kev = merged
            .decision_providers
            .iter()
            .find(|p| p.name == "kev")
            .unwrap();
        assert_eq!(
            kev.base_url, "http://127.0.0.1:9009",
            "локальный слой сильнее"
        );
        assert!(merged.decision_providers.iter().any(|p| p.name == "jev"));

        let empty = crate::config::merge(Default::default(), Default::default());
        assert!(empty.decision_providers.is_empty(), "секция опциональна");
    }

    #[test]
    fn golden_process_parses() {
        berimor_process_engine::parser::parse(include_str!(
            "../../../fixtures/golden/processes/decision-routing.yaml"
        ))
        .unwrap();
    }
}
