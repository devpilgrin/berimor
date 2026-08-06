//! `oauth` — вход по подписке через OAuth PKCE (RFC 7636) с loopback-редиректом.
//!
//! Источник: ADR-0027 (`docs/ADR/0027-oauth-subscription-login-pkce.md`),
//! ROADMAP §20.25. Границы:
//! - I4: токены — секреты, хранятся ТОЛЬКО в `secrets.env` (0600) и никогда
//!   не попадают в журналы, ошибки и вывод (в сообщениях — имена провайдеров
//!   и HTTP-статусы, не значения);
//! - I5: OAuth-логин опционален, API-ключи остаются полноценным путём;
//! - детерминизм: весь token lifecycle (обмен, refresh, отзыв) — кодом.
//!
//! СЛЕДУЮЩИЙ ШАГ (не входит в v1): подключение OAuth-профиля к Model Pool —
//! запись провайдера вида `[[providers]] auth = "oauth"` в конфиге, читающая
//! access-токен через [`access_token`] вместо `api_key_env`. Сейчас работают
//! `login`/`logout`/`--list`, прозрачный refresh и реестр токенов.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use sha2::{Digest, Sha256};

// ============================================================================
// ПУБЛИЧНЫЕ ЭНДПОИНТЫ ПРОВАЙДЕРОВ — ТРЕБУЮТ РУЧНОЙ ПРОВЕРКИ (manual).
// Источник значений: публичные client_id/endpoint'ы Claude Code и Codex CLI,
// сверенные с реализацией jcode (github.com/1jehuang/jcode,
// crates/jcode-base/src/auth/oauth.rs, снапшот 2026-08-06). Провайдеры
// вправе менять endpoint'ы и client_id — перед релизом значения ниже
// проверяются на живых аккаунтах по ручному чек-листу (ADR-0027 «Тесты»).
// Сам flow (PKCE, loopback, обмен, refresh) от этих констант не зависит и
// полностью проверен моками в тестах этого модуля.
// ============================================================================

/// Профиль OAuth-провайдера: публичный клиент (без client_secret — PKCE
/// заменяет его, RFC 7636 §1.1).
pub struct ProviderProfile {
    /// Каноническое имя (`claude`, `openai`).
    pub name: &'static str,
    pub client_id: &'static str,
    pub authorize_url: &'static str,
    pub token_url: &'static str,
    pub scopes: &'static str,
    /// Редирект по умолчанию: Claude subscription-flow построен на
    /// ручной вставке кода со страницы колбэка, Codex — на loopback.
    pub default_redirect: RedirectMode,
    /// Формат тела token-endpoint'а: Anthropic ждёт JSON (со state),
    /// OpenAI — application/x-www-form-urlencoded.
    pub token_body: TokenBodyFormat,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RedirectMode {
    /// `http://127.0.0.1:<эфемерный порт>/callback` — слушаем сами.
    Loopback,
    /// Фиксированная страница провайдера, код вставляется вручную.
    ManualFixed(&'static str),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TokenBodyFormat {
    /// JSON `{grant_type, code, redirect_uri, client_id, code_verifier, state}`
    /// (Anthropic; state обязателен в теле — см. jcode exchange_claude_code).
    Json,
    /// form-urlencoded `grant_type&client_id&code&code_verifier&redirect_uri`
    /// (Codex CLI).
    Form,
}

/// MANUAL-CHECK (релизный чек-лист ADR-0027): актуальность client_id и URL.
const CLAUDE: ProviderProfile = ProviderProfile {
    name: "claude",
    client_id: "9d1c250a-e61b-44d9-88ed-5944d1962f5e",
    authorize_url: "https://claude.com/cai/oauth/authorize",
    token_url: "https://platform.claude.com/v1/oauth/token",
    scopes: "org:create_api_key user:profile user:inference user:sessions:claude_code user:mcp_servers user:file_upload",
    default_redirect: RedirectMode::ManualFixed("https://platform.claude.com/oauth/code/callback"),
    token_body: TokenBodyFormat::Json,
};

/// MANUAL-CHECK (релизный чек-лист ADR-0027): актуальность client_id и URL.
const OPENAI: ProviderProfile = ProviderProfile {
    name: "openai",
    client_id: "app_EMoamEEZ73f0CkXaXp7hrann",
    authorize_url: "https://auth.openai.com/oauth/authorize",
    token_url: "https://auth.openai.com/oauth/token",
    scopes: "openid profile email offline_access",
    default_redirect: RedirectMode::Loopback,
    token_body: TokenBodyFormat::Form,
};

/// Таймаут ожидания колбэка на loopback-listener (ADR-0027: «короткий»).
const CALLBACK_TIMEOUT: Duration = Duration::from_secs(180);
/// Запас к истечению: токен с остатком жизни < SKEW считается истёкшим,
/// чтобы не уйти в запрос с токеном, который умрёт посреди ответа.
const EXPIRY_SKEW_SECS: u64 = 60;
/// Таймаут HTTP-вызовов token-endpoint'а.
const TOKEN_HTTP_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, thiserror::Error)]
pub enum OAuthError {
    #[error("неизвестный OAuth-провайдер «{0}» (доступны: claude, openai)")]
    UnknownProvider(String),
    #[error("имя провайдера «{0}» недопустимо (нужны [a-z0-9-]: иначе ключи реестра небезопасны)")]
    BadProviderName(String),
    #[error("не удалось определить глобальную директорию конфигурации")]
    NoGlobalDir,
    #[error("генератор случайных чисел недоступен")]
    Rng,
    #[error("state из колбэка не совпал с отправленным — вход отклонён (CSRF-защита); начните login заново")]
    StateMismatch,
    #[error("колбэк не содержал code (провайдер вернул ошибку авторизации)")]
    CallbackMissingCode,
    #[error("колбэк не получен за {} с", .0.as_secs())]
    CallbackTimeout(Duration),
    // В v1 конструируется только из access_token (integration surface
    // следующего шага); остальные ветки активны уже сейчас.
    #[allow(dead_code)]
    #[error("OAuth-профиль «{0}» не найден в реестре секретов (выполните `berimor login`)")]
    NotLoggedIn(String),
    #[error("у OAuth-профиля «{0}» нет refresh-токена — требуется повторный login")]
    NoRefreshToken(String),
    #[error("token-endpoint провайдера «{provider}» ответил HTTP {status}")]
    TokenEndpoint { provider: String, status: u16 },
    #[error("token-endpoint провайдера «{0}» недоступен или вернул не-JSON")]
    TokenNetwork(String),
    #[error("пустой код авторизации")]
    EmptyCode,
    #[error("ввод/вывод: {0}")]
    Io(#[from] std::io::Error),
}

/// Профиль по имени (манифест из двух записей v1).
pub fn profile(name: &str) -> Result<&'static ProviderProfile, OAuthError> {
    match name {
        "claude" => Ok(&CLAUDE),
        "openai" => Ok(&OPENAI),
        other => Err(OAuthError::UnknownProvider(other.to_string())),
    }
}

// ---------------------------------------------------------------------------
// PKCE (RFC 7636)
// ---------------------------------------------------------------------------

/// Пара PKCE + anti-CSRF state. `verifier` никому не показывается до обмена.
pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
    pub state: String,
}

fn random_urlsafe(bytes: usize) -> Result<String, OAuthError> {
    let mut buf = vec![0u8; bytes];
    getrandom::fill(&mut buf).map_err(|_| OAuthError::Rng)?;
    Ok(URL_SAFE_NO_PAD.encode(buf))
}

/// RFC 7636 §4.1/§4.2: verifier — 43 символа base64url из 32 байт
/// случайности; challenge = BASE64URL(SHA256(verifier)) без padding.
pub fn generate_pkce() -> Result<Pkce, OAuthError> {
    let verifier = random_urlsafe(32)?;
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let state = random_urlsafe(16)?;
    Ok(Pkce {
        verifier,
        challenge,
        state,
    })
}

/// Строгая проверка state (ADR-0027: «validate state strictly»).
pub fn validate_state(expected: &str, got: &str) -> Result<(), OAuthError> {
    if expected == got {
        Ok(())
    } else {
        Err(OAuthError::StateMismatch)
    }
}

/// RFC 3986 unreserved только — для query-параметров authorize-URL.
fn url_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// URL авторизации с PKCE-параметрами.
pub fn authorization_url(
    profile: &ProviderProfile,
    redirect_uri: &str,
    challenge: &str,
    state: &str,
) -> String {
    format!(
        "{}?code=true&client_id={}&response_type=code&redirect_uri={}&scope={}&code_challenge={}&code_challenge_method=S256&state={}",
        profile.authorize_url,
        profile.client_id,
        url_encode(redirect_uri),
        url_encode(profile.scopes),
        challenge,
        state,
    )
}

// ---------------------------------------------------------------------------
// Loopback-listener и разбор пользовательского ввода
// ---------------------------------------------------------------------------

/// Принимает ОДИН колбэк на уже привязанном listener'е за `timeout`.
/// Читает request-line `GET /callback?code=…&state=…`, строго сверяет
/// state, отвечает браузеру понятной страницей. Возвращает code.
pub fn wait_for_callback(
    listener: &TcpListener,
    expected_state: &str,
    timeout: Duration,
) -> Result<String, OAuthError> {
    listener.set_nonblocking(true)?;
    let deadline = std::time::Instant::now() + timeout;
    let stream = loop {
        match listener.accept() {
            Ok((stream, _)) => break stream,
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                if std::time::Instant::now() >= deadline {
                    return Err(OAuthError::CallbackTimeout(timeout));
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(err) => return Err(err.into()),
        }
    };
    let mut reader = BufReader::new(stream);
    let mut request_line = String::new();
    reader.read_line(&mut request_line)?;
    let target = request_line
        .split_whitespace()
        .nth(1)
        .unwrap_or_default()
        .to_string();
    // Заголовки до пустой строки — читаем, чтобы браузер не оборвался раньше ответа.
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 || line.trim().is_empty() {
            break;
        }
    }
    let query = target.split_once('?').map(|(_, q)| q).unwrap_or("");
    let mut code = None;
    let mut state = None;
    for pair in query.split('&') {
        if let Some((key, value)) = pair.split_once('=') {
            match key {
                "code" => code = Some(percent_decode(value)),
                "state" => state = Some(percent_decode(value)),
                _ => {}
            }
        }
    }
    let outcome: Result<String, OAuthError> = match (code, state) {
        (Some(code), Some(state)) => validate_state(expected_state, &state).map(|()| code),
        _ => Err(OAuthError::CallbackMissingCode),
    };
    let (status, text) = match &outcome {
        Ok(_) => ("200 OK", "Вход выполнен — вкладку можно закрыть."),
        Err(_) => (
            "400 Bad Request",
            "Ошибка входа — вернитесь в терминал за деталями.",
        ),
    };
    let body = format!("<html><body><p>{text}</p></body></html>");
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = reader.get_mut().write_all(response.as_bytes());
    outcome
}

/// Минимальный percent-decode для query-параметров колбэка (коды и state —
/// base64url, но провайдер вправе вернуть и закодированные символы).
fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(hex) = u8::from_str_radix(&value[i + 1..i + 3], 16) {
                out.push(hex);
                i += 3;
                continue;
            }
        }
        out.push(if bytes[i] == b'+' { b' ' } else { bytes[i] });
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Ручной ввод кода (headless-путь ADR-0027): принимает «голый» code,
/// полный URL колбэка (`…?code=…&state=…`) или формат `code#state`
/// (страница Claude показывает именно его). Возвращает (code, state?).
pub fn parse_code_input(input: &str) -> Result<(String, Option<String>), OAuthError> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(OAuthError::EmptyCode);
    }
    let (raw, state_from_query) = if trimmed.contains("code=") {
        let query = trimmed.split_once('?').map(|(_, q)| q).unwrap_or(trimmed);
        let mut code = None;
        let mut state = None;
        for pair in query.split('&') {
            if let Some((key, value)) = pair.split_once('=') {
                match key {
                    "code" => code = Some(percent_decode(value)),
                    "state" => state = Some(percent_decode(value)),
                    _ => {}
                }
            }
        }
        (code.ok_or(OAuthError::EmptyCode)?, state)
    } else {
        (trimmed.to_string(), None)
    };
    let (code, state) = match raw.split_once('#') {
        Some((code, state)) => (code.to_string(), Some(state.to_string())),
        None => (raw, state_from_query),
    };
    if code.is_empty() {
        return Err(OAuthError::EmptyCode);
    }
    Ok((code, state))
}

// ---------------------------------------------------------------------------
// Обмен кода и refresh (token-endpoint)
// ---------------------------------------------------------------------------

/// Набор токенов после обмена/refresh. Значения — секреты I4: не
/// сериализуются в журналы, не включаются в ошибки.
pub struct TokenSet {
    pub access_token: String,
    /// При refresh провайдер может не вернуть новый refresh-токен —
    /// тогда остаётся прежний (None здесь означает «не менять»).
    pub refresh_token: Option<String>,
    pub expires_at_unix: u64,
}

/// I4: Debug маскирован (тот же принцип, что berimor_secrets::Secret) —
/// unwrap_err в тестах и случайный `{tokens:?}` не утекают значениями.
impl std::fmt::Debug for TokenSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "TokenSet(‹masked›, expires_at={})", self.expires_at_unix)
    }
}

#[derive(serde::Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: Option<u64>,
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn parse_token_response(
    profile_name: &str,
    response: reqwest::blocking::Response,
) -> Result<TokenSet, OAuthError> {
    let status = response.status();
    if !status.is_success() {
        // Тело ответа НЕ читаем в ошибку: оно может содержать детали,
        // которые мы не контролируем (I4 — статуса достаточно).
        return Err(OAuthError::TokenEndpoint {
            provider: profile_name.to_string(),
            status: status.as_u16(),
        });
    }
    let parsed: TokenResponse = response
        .json()
        .map_err(|_| OAuthError::TokenNetwork(profile_name.to_string()))?;
    Ok(TokenSet {
        access_token: parsed.access_token,
        refresh_token: parsed.refresh_token,
        expires_at_unix: now_unix() + parsed.expires_in.unwrap_or(3600),
    })
}

fn http_client() -> Result<reqwest::blocking::Client, OAuthError> {
    reqwest::blocking::Client::builder()
        .timeout(TOKEN_HTTP_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| OAuthError::TokenNetwork("http-client".to_string()))
}

/// Обмен authorization-code на токены (RFC 7636 §4.5).
pub fn exchange_code(
    profile: &ProviderProfile,
    token_url: &str,
    code: &str,
    pkce: &Pkce,
    redirect_uri: &str,
) -> Result<TokenSet, OAuthError> {
    let client = http_client()?;
    let request = match profile.token_body {
        TokenBodyFormat::Json => client.post(token_url).json(&serde_json::json!({
            "grant_type": "authorization_code",
            "code": code,
            "redirect_uri": redirect_uri,
            "client_id": profile.client_id,
            "code_verifier": pkce.verifier,
            "state": pkce.state,
        })),
        TokenBodyFormat::Form => client.post(token_url).form(&[
            ("grant_type", "authorization_code"),
            ("client_id", profile.client_id),
            ("code", code),
            ("code_verifier", pkce.verifier.as_str()),
            ("redirect_uri", redirect_uri),
        ]),
    };
    let response = request
        .send()
        .map_err(|_| OAuthError::TokenNetwork(profile.name.to_string()))?;
    parse_token_response(profile.name, response)
}

/// Refresh access-токена (RFC 6749 §6). Секреты — только в теле запроса к
/// endpoint'у провайдера, нигде больше (ADR-0027 «Альтернативы»).
#[allow(dead_code)] // v1: вызывается из access_token и тестов.
pub fn refresh_tokens(
    profile_name: &str,
    token_url: &str,
    client_id: &str,
    body: TokenBodyFormat,
    refresh_token: &str,
) -> Result<TokenSet, OAuthError> {
    let client = http_client()?;
    let request = match body {
        TokenBodyFormat::Json => client.post(token_url).json(&serde_json::json!({
            "grant_type": "refresh_token",
            "client_id": client_id,
            "refresh_token": refresh_token,
        })),
        TokenBodyFormat::Form => client.post(token_url).form(&[
            ("grant_type", "refresh_token"),
            ("client_id", client_id),
            ("refresh_token", refresh_token),
        ]),
    };
    let response = request
        .send()
        .map_err(|_| OAuthError::TokenNetwork(profile_name.to_string()))?;
    parse_token_response(profile_name, response)
}

// ---------------------------------------------------------------------------
// Реестр секретов: запись/чтение/удаление OAuth-записей (I4, 0600)
// ---------------------------------------------------------------------------

fn key_prefix(provider: &str) -> Result<String, OAuthError> {
    let mut name = String::with_capacity(provider.len());
    for ch in provider.chars() {
        if ch.is_ascii_lowercase() || ch.is_ascii_digit() {
            name.push(ch.to_ascii_uppercase());
        } else if ch == '-' {
            name.push('_');
        } else {
            return Err(OAuthError::BadProviderName(provider.to_string()));
        }
    }
    Ok(format!("BERIMOR_OAUTH_{name}_"))
}

/// Запись OAuth-профиля из реестра. Поля плоские KEY=value — тот же
/// формат, что у API-ключей (`secrets.env`, config.rs::parse_secrets_env).
pub struct TokenRecord {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at_unix: u64,
    pub token_url: String,
    pub client_id: String,
    pub body_format: TokenBodyFormat,
}

/// Сохраняет/заменяет запись провайдера в `secrets.env` (0600). Перезапись
/// осознанная — в отличие от первичных ключей (`setup::append_secret`),
/// OAuth-токены обновляются кодом при каждом refresh.
pub fn store_record(
    secrets_path: &Path,
    provider: &str,
    record: &TokenRecord,
) -> Result<(), OAuthError> {
    let prefix = key_prefix(provider)?;
    let existing = std::fs::read_to_string(secrets_path).unwrap_or_default();
    // Чужие строки (включая комментарии) сохраняются дословно; наши ключи
    // данного провайдера вырезаются и дописываются новыми значениями.
    let mut kept: Vec<&str> = existing
        .lines()
        .filter(|line| {
            let trimmed = line.trim();
            !(trimmed.starts_with(&prefix) && trimmed.contains('='))
        })
        .collect();
    let format_flag = match record.body_format {
        TokenBodyFormat::Json => "json",
        TokenBodyFormat::Form => "form",
    };
    let block = format!(
        "{prefix}ACCESS_TOKEN={}\n{prefix}REFRESH_TOKEN={}\n{prefix}EXPIRES_AT={}\n{prefix}TOKEN_URL={}\n{prefix}CLIENT_ID={}\n{prefix}BODY_FORMAT={format_flag}\n",
        record.access_token,
        record.refresh_token,
        record.expires_at_unix,
        record.token_url,
        record.client_id,
    );
    kept.push(block.trim_end_matches('\n'));
    write_registry(secrets_path, &kept.join("\n"))
}

/// Читает запись провайдера; отсутствие — None (не ошибка: просто не залогинен).
pub fn load_record(secrets_path: &Path, provider: &str) -> Result<Option<TokenRecord>, OAuthError> {
    let prefix = key_prefix(provider)?;
    let Ok(contents) = std::fs::read_to_string(secrets_path) else {
        return Ok(None);
    };
    let entries = crate::config::parse_secrets_env(&contents);
    let get = |suffix: &str| {
        entries
            .iter()
            .find(|(name, _)| name == &format!("{prefix}{suffix}"))
            .map(|(_, value)| value.clone())
    };
    let (Some(access_token), Some(token_url), Some(client_id)) =
        (get("ACCESS_TOKEN"), get("TOKEN_URL"), get("CLIENT_ID"))
    else {
        return Ok(None);
    };
    let body_format = match get("BODY_FORMAT").as_deref() {
        Some("form") => TokenBodyFormat::Form,
        _ => TokenBodyFormat::Json,
    };
    Ok(Some(TokenRecord {
        access_token,
        refresh_token: get("REFRESH_TOKEN").unwrap_or_default(),
        expires_at_unix: get("EXPIRES_AT").and_then(|v| v.parse().ok()).unwrap_or(0),
        token_url,
        client_id,
        body_format,
    }))
}

/// Удаляет запись провайдера (logout). Возвращает true, если запись была.
pub fn remove_record(secrets_path: &Path, provider: &str) -> Result<bool, OAuthError> {
    let prefix = key_prefix(provider)?;
    let Ok(existing) = std::fs::read_to_string(secrets_path) else {
        return Ok(false);
    };
    let kept: Vec<&str> = existing
        .lines()
        .filter(|line| {
            let trimmed = line.trim();
            !(trimmed.starts_with(&prefix) && trimmed.contains('='))
        })
        .collect();
    if kept.len() == existing.lines().count() {
        return Ok(false);
    }
    write_registry(secrets_path, &kept.join("\n"))?;
    Ok(true)
}

fn write_registry(secrets_path: &Path, contents: &str) -> Result<(), OAuthError> {
    if let Some(parent) = secrets_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(secrets_path, format!("{contents}\n"))?;
    // 0600: реестр с refresh-токенами читается только владельцем — тот же
    // уровень, что ~/.ssh/id_* (setup.rs::append_secret, security-model I4).
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(secrets_path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Прозрачный refresh: единая точка получения валидного access-токена
// ---------------------------------------------------------------------------

/// Возвращает валидный access-токен провайдера, прозрачно обновляя его при
/// истечении (с запасом EXPIRY_SKEW_SECS). Обновление журналируется в
/// stderr БЕЗ значений токенов (ADR-0027 п.2). Endpoint и client_id берутся
/// из записи реестра — токены не покидают машину никуда, кроме endpoint'а
/// провайдера, записанного при login.
///
/// СЛЕДУЮЩИЙ ШАГ: HTTP-провайдер Model Pool будет вызывать эту функцию для
/// записей `auth = "oauth"` вместо чтения `api_key_env`.
#[allow(dead_code)] // v1: integration surface для Model Pool; покрыто тестами.
pub fn access_token(secrets_path: &Path, provider: &str) -> Result<String, OAuthError> {
    let record = load_record(secrets_path, provider)?
        .ok_or_else(|| OAuthError::NotLoggedIn(provider.to_string()))?;
    if record.expires_at_unix > now_unix() + EXPIRY_SKEW_SECS {
        return Ok(record.access_token);
    }
    if record.refresh_token.is_empty() {
        return Err(OAuthError::NoRefreshToken(provider.to_string()));
    }
    eprintln!("[berimor] oauth: access-токен «{provider}» истёк — обновляю по refresh-токену");
    let refreshed = refresh_tokens(
        provider,
        &record.token_url,
        &record.client_id,
        record.body_format,
        &record.refresh_token,
    )?;
    let updated = TokenRecord {
        access_token: refreshed.access_token,
        refresh_token: refreshed.refresh_token.unwrap_or(record.refresh_token),
        expires_at_unix: refreshed.expires_at_unix,
        token_url: record.token_url,
        client_id: record.client_id,
        body_format: record.body_format,
    };
    store_record(secrets_path, provider, &updated)?;
    eprintln!("[berimor] oauth: access-токен «{provider}» обновлён");
    Ok(updated.access_token)
}

// ---------------------------------------------------------------------------
// CLI-оркестрация: login / logout / list
// ---------------------------------------------------------------------------

fn secrets_path() -> Result<PathBuf, OAuthError> {
    crate::config::secrets_env_path().ok_or(OAuthError::NoGlobalDir)
}

/// Best-effort открытие браузера (ADR-0027: не принудительно — URL всегда
/// напечатан, браузер только помогает).
fn try_open_browser(url: &str) {
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(target_os = "windows") {
        "rundll32"
    } else {
        "xdg-open"
    };
    let args: Vec<&str> = if cfg!(target_os = "windows") {
        vec!["url.dll,FileProtocolHandler", url]
    } else {
        vec![url]
    };
    let _ = std::process::Command::new(opener)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

fn read_line(prompt: &str) -> Result<String, OAuthError> {
    eprint!("{prompt}");
    std::io::stderr().flush()?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    Ok(line.trim().to_string())
}

/// `berimor login --provider <name> [--manual]` — полный PKCE-flow.
pub fn login(provider_name: &str, manual: bool) -> Result<(), OAuthError> {
    let profile = profile(provider_name)?;
    let path = secrets_path()?;
    let pkce = generate_pkce()?;

    // Режим редиректа: --manual принудительно, иначе — умолчание провайдера
    // (Claude — ручная вставка, OpenAI — loopback).
    let mode = if manual {
        match profile.default_redirect {
            RedirectMode::ManualFixed(url) => RedirectMode::ManualFixed(url),
            RedirectMode::Loopback => {
                RedirectMode::ManualFixed("http://localhost:1455/auth/callback")
            }
        }
    } else {
        profile.default_redirect
    };

    let (code, code_state, redirect_uri) = match mode {
        RedirectMode::Loopback => {
            let listener = TcpListener::bind("127.0.0.1:0")?;
            let port = listener.local_addr()?.port();
            let redirect_uri = format!("http://127.0.0.1:{port}/callback");
            let url = authorization_url(profile, &redirect_uri, &pkce.challenge, &pkce.state);
            println!("{url}");
            eprintln!("[berimor] откройте URL выше в браузере (пытаюсь открыть автоматически)…");
            try_open_browser(&url);
            eprintln!(
                "[berimor] жду колбэк на 127.0.0.1:{port} ({} с; Ctrl-C — отмена)…",
                CALLBACK_TIMEOUT.as_secs()
            );
            let code = wait_for_callback(&listener, &pkce.state, CALLBACK_TIMEOUT)?;
            (code, None, redirect_uri)
        }
        RedirectMode::ManualFixed(redirect) => {
            let url = authorization_url(profile, redirect, &pkce.challenge, &pkce.state);
            println!("{url}");
            eprintln!("[berimor] откройте URL выше в браузере (пытаюсь открыть автоматически)…");
            try_open_browser(&url);
            let input = read_line(
                "Вставьте код со страницы провайдера (код, URL колбэка или code#state): ",
            )?;
            let (code, state) = parse_code_input(&input)?;
            (code, state, redirect.to_string())
        }
    };

    // Если страница провайдера вернула state в явном виде — сверяем строго.
    if let Some(returned) = code_state.as_deref() {
        validate_state(&pkce.state, returned)?;
    }

    // redirect_uri в обмене обязан совпадать с тем, что был в authorize-URL.
    let tokens = exchange_code(profile, profile.token_url, &code, &pkce, &redirect_uri)?;
    finish_login(&path, profile, tokens)
}

fn finish_login(
    path: &Path,
    profile: &ProviderProfile,
    tokens: TokenSet,
) -> Result<(), OAuthError> {
    let Some(refresh_token) = tokens.refresh_token.clone() else {
        return Err(OAuthError::NoRefreshToken(profile.name.to_string()));
    };
    let record = TokenRecord {
        access_token: tokens.access_token,
        refresh_token,
        expires_at_unix: tokens.expires_at_unix,
        token_url: profile.token_url.to_string(),
        client_id: profile.client_id.to_string(),
        body_format: profile.token_body,
    };
    store_record(path, profile.name, &record)?;
    eprintln!(
        "[berimor] oauth: профиль «{}» сохранён в {} (права 0600; токены не выводятся)",
        profile.name,
        path.display()
    );
    Ok(())
}

/// `berimor logout --provider <name>` — удаление записи из реестра.
/// (Revoke у провайдера — следующий шаг, ADR-0027 «Последствия» помечает
/// его best-effort; v1 ограничивается локальным отзывом.)
pub fn logout(provider_name: &str) -> Result<bool, OAuthError> {
    let path = secrets_path()?;
    let removed = remove_record(&path, provider_name)?;
    if removed {
        eprintln!("[berimor] oauth: профиль «{provider_name}» удалён из реестра секретов");
    } else {
        eprintln!("[berimor] oauth: профиль «{provider_name}» не найден — нечего удалять");
    }
    Ok(removed)
}

/// Статус одного OAuth-профиля для `--list` (без значений токенов — I4).
pub struct ProfileStatus {
    pub provider: String,
    pub expires_at_unix: u64,
    pub expired: bool,
    pub has_refresh: bool,
    pub token_url: String,
}

/// `berimor login --list` — перечень OAuth-профилей в реестре.
pub fn list() -> Result<Vec<ProfileStatus>, OAuthError> {
    let Some(path) = crate::config::secrets_env_path() else {
        return Ok(Vec::new());
    };
    let Ok(contents) = std::fs::read_to_string(&path) else {
        return Ok(Vec::new());
    };
    let mut providers: Vec<String> = crate::config::parse_secrets_env(&contents)
        .iter()
        .filter_map(|(name, _)| {
            name.strip_prefix("BERIMOR_OAUTH_")
                .and_then(|rest| rest.strip_suffix("_ACCESS_TOKEN"))
                .map(|p| p.to_ascii_lowercase().replace('_', "-"))
        })
        .collect();
    providers.sort();
    providers.dedup();
    let mut out = Vec::new();
    for provider in providers {
        if let Some(record) = load_record(&path, &provider)? {
            out.push(ProfileStatus {
                provider,
                expires_at_unix: record.expires_at_unix,
                expired: record.expires_at_unix <= now_unix() + EXPIRY_SKEW_SECS,
                has_refresh: !record.refresh_token.is_empty(),
                token_url: record.token_url,
            });
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Тесты: мок token-endpoint'ов локальным HTTP-сервером, без реальных
// аккаунтов (стратегия ADR-0027; паттерн — http_provider.rs::serve_once).
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("berimor-oauth-test-{}-{}", std::process::id(), tag));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Минимальный HTTP-сервер на std: один запрос → заданный ответ.
    /// Возвращает (url, handle с захваченным запросом).
    fn serve_once(status: &str, body: &str) -> (String, std::thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let status = status.to_string();
        let body = body.to_string();
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

    fn test_profile(body: TokenBodyFormat) -> ProviderProfile {
        ProviderProfile {
            name: "mockprov",
            client_id: "mock-client",
            authorize_url: "http://127.0.0.1/authorize",
            token_url: "http://127.0.0.1/token",
            scopes: "scope-a scope-b",
            default_redirect: RedirectMode::Loopback,
            token_body: body,
        }
    }

    fn pkce_fixture() -> Pkce {
        Pkce {
            verifier: "verifier-abc".into(),
            challenge: "challenge-abc".into(),
            state: "state-xyz".into(),
        }
    }

    // --- PKCE unit-тесты ---

    /// RFC 7636 Appendix B — известный вектор S256.
    #[test]
    fn s256_challenge_matches_rfc7636_vector() {
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        assert_eq!(challenge, "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
    }

    #[test]
    fn generated_verifier_and_state_are_urlsafe_and_long_enough() {
        let pkce = generate_pkce().unwrap();
        // 32 байта → 43 символа base64url без padding (RFC 7636 §4.1).
        assert_eq!(pkce.verifier.len(), 43);
        let urlsafe = |s: &str| {
            s.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        };
        assert!(urlsafe(&pkce.verifier));
        assert!(urlsafe(&pkce.challenge));
        assert!(urlsafe(&pkce.state));
        assert!(!pkce.state.is_empty());
    }

    #[test]
    fn state_mismatch_is_rejected() {
        assert!(validate_state("abc", "abc").is_ok());
        assert!(matches!(
            validate_state("abc", "abd"),
            Err(OAuthError::StateMismatch)
        ));
    }

    #[test]
    fn authorization_url_carries_pkce_parameters() {
        let profile = test_profile(TokenBodyFormat::Json);
        let url = authorization_url(&profile, "http://127.0.0.1:9/callback", "CH", "ST");
        assert!(url.contains("code_challenge=CH"));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains("state=ST"));
        assert!(url.contains("client_id=mock-client"));
        // Пробелы скоупов закодированы.
        assert!(url.contains("scope=scope-a%20scope-b"));
        assert!(url.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A9%2Fcallback"));
    }

    // --- разбор ручного ввода ---

    #[test]
    fn parse_input_accepts_plain_code_url_and_code_hash_state() {
        assert_eq!(
            parse_code_input("plain-code").unwrap(),
            ("plain-code".to_string(), None)
        );
        assert_eq!(
            parse_code_input("https://platform.claude.com/oauth/code/callback?code=abc&state=xyz")
                .unwrap(),
            ("abc".to_string(), Some("xyz".to_string()))
        );
        assert_eq!(
            parse_code_input("abc#xyz").unwrap(),
            ("abc".to_string(), Some("xyz".to_string()))
        );
        assert!(matches!(
            parse_code_input("   "),
            Err(OAuthError::EmptyCode)
        ));
    }

    // --- loopback-listener ---

    /// Клиент-заглушка: шлёт GET с заданным target на listener.
    fn send_callback(addr: std::net::SocketAddr, target: &str) {
        let mut stream = std::net::TcpStream::connect(addr).unwrap();
        stream
            .write_all(format!("GET {target} HTTP/1.1\r\nHost: localhost\r\n\r\n").as_bytes())
            .unwrap();
        // Дочитываем ответ, чтобы сервер завершил запись.
        let mut buf = Vec::new();
        let _ = stream.read_to_end(&mut buf);
    }

    #[test]
    fn loopback_callback_returns_code_on_matching_state() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let client = std::thread::spawn(move || {
            send_callback(addr, "/callback?code=the-code&state=expected");
        });
        let code = wait_for_callback(&listener, "expected", Duration::from_secs(5)).unwrap();
        client.join().unwrap();
        assert_eq!(code, "the-code");
    }

    #[test]
    fn loopback_callback_rejects_wrong_state() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let client = std::thread::spawn(move || {
            send_callback(addr, "/callback?code=the-code&state=attacker");
        });
        let result = wait_for_callback(&listener, "expected", Duration::from_secs(5));
        client.join().unwrap();
        assert!(matches!(result, Err(OAuthError::StateMismatch)));
    }

    #[test]
    fn loopback_callback_times_out() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let result = wait_for_callback(&listener, "expected", Duration::from_millis(200));
        assert!(matches!(result, Err(OAuthError::CallbackTimeout(_))));
    }

    // --- обмен кода (мок token-endpoint) ---

    #[test]
    fn exchange_code_posts_pkce_fields_and_parses_tokens() {
        let (url, server) = serve_once(
            "200 OK",
            r#"{"access_token":"acc-1","refresh_token":"ref-1","expires_in":3600}"#,
        );
        let profile = test_profile(TokenBodyFormat::Json);
        let tokens =
            exchange_code(&profile, &url, "code-42", &pkce_fixture(), "http://r/cb").unwrap();
        let request = server.join().unwrap();
        assert_eq!(tokens.access_token, "acc-1");
        assert_eq!(tokens.refresh_token.as_deref(), Some("ref-1"));
        assert!(tokens.expires_at_unix > now_unix());
        // JSON-ветка (Anthropic): verifier и state — в теле.
        assert!(request.contains("\"code_verifier\":\"verifier-abc\""));
        assert!(request.contains("\"state\":\"state-xyz\""));
        assert!(request.contains("\"client_id\":\"mock-client\""));
        assert!(request.contains("\"code\":\"code-42\""));
    }

    #[test]
    fn exchange_code_form_branch_urlencodes_fields() {
        let (url, server) = serve_once(
            "200 OK",
            r#"{"access_token":"acc-2","refresh_token":"ref-2","expires_in":60}"#,
        );
        let profile = test_profile(TokenBodyFormat::Form);
        let tokens =
            exchange_code(&profile, &url, "code-7", &pkce_fixture(), "http://r/cb").unwrap();
        let request = server.join().unwrap();
        assert_eq!(tokens.access_token, "acc-2");
        assert!(request.contains("grant_type=authorization_code"));
        assert!(request.contains("code_verifier=verifier-abc"));
        assert!(request.contains("code=code-7"));
        assert!(request.contains("redirect_uri=http%3A%2F%2Fr%2Fcb"));
    }

    #[test]
    fn exchange_code_endpoint_400_is_error_without_secret_leak() {
        let (url, server) = serve_once("400 Bad Request", r#"{"error":"invalid_grant"}"#);
        let profile = test_profile(TokenBodyFormat::Json);
        let err =
            exchange_code(&profile, &url, "bad-code", &pkce_fixture(), "http://r/cb").unwrap_err();
        server.join().unwrap();
        let message = err.to_string();
        assert!(matches!(err, OAuthError::TokenEndpoint { status: 400, .. }));
        // В ошибке — статус и имя провайдера, не код/verifier.
        assert!(!message.contains("bad-code"));
        assert!(!message.contains("verifier-abc"));
    }

    #[test]
    fn exchange_code_network_down_is_error() {
        let profile = test_profile(TokenBodyFormat::Json);
        // Порт 1: connect refused, реальный сетевой сбой.
        let err = exchange_code(
            &profile,
            "http://127.0.0.1:1/token",
            "code",
            &pkce_fixture(),
            "http://r/cb",
        )
        .unwrap_err();
        assert!(matches!(err, OAuthError::TokenNetwork(_)));
        assert!(!err.to_string().contains("verifier-abc"));
    }

    // --- хранение и refresh ---

    fn record_fixture(expires_at: u64) -> TokenRecord {
        TokenRecord {
            access_token: "stored-access".into(),
            refresh_token: "stored-refresh".into(),
            expires_at_unix: expires_at,
            token_url: "http://127.0.0.1/token".into(),
            client_id: "mock-client".into(),
            body_format: TokenBodyFormat::Json,
        }
    }

    #[test]
    fn store_then_load_roundtrip_preserves_other_keys() {
        let dir = temp_dir("roundtrip");
        let path = dir.join("secrets.env");
        std::fs::write(&path, "# комментарий\nOPENAI_API_KEY=sk-keep-me\n").unwrap();
        store_record(&path, "claude", &record_fixture(now_unix() + 3600)).unwrap();
        let loaded = load_record(&path, "claude").unwrap().unwrap();
        assert_eq!(loaded.access_token, "stored-access");
        assert_eq!(loaded.refresh_token, "stored-refresh");
        assert_eq!(loaded.client_id, "mock-client");
        // Чужие ключи и комментарии уцелели.
        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.contains("# комментарий"));
        assert!(contents.contains("OPENAI_API_KEY=sk-keep-me"));
        // 0600 на unix.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Повторное сохранение (login заново / refresh) ЗАМЕНЯЕТ запись, а не
    /// плодит дубликаты — refresh-токен живёт дольше одного access-токена.
    #[test]
    fn store_replaces_existing_record_without_duplicates() {
        let dir = temp_dir("replace");
        let path = dir.join("secrets.env");
        store_record(&path, "openai", &record_fixture(now_unix() + 10)).unwrap();
        let mut newer = record_fixture(now_unix() + 7200);
        newer.access_token = "fresh-access".into();
        store_record(&path, "openai", &newer).unwrap();
        let contents = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            contents
                .matches("BERIMOR_OAUTH_OPENAI_ACCESS_TOKEN=")
                .count(),
            1
        );
        assert!(contents.contains("fresh-access"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn access_token_returns_stored_when_fresh() {
        let dir = temp_dir("fresh");
        let path = dir.join("secrets.env");
        store_record(&path, "claude", &record_fixture(now_unix() + 3600)).unwrap();
        assert_eq!(access_token(&path, "claude").unwrap(), "stored-access");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn access_token_refreshes_when_expired_and_persists() {
        let (url, server) = serve_once(
            "200 OK",
            r#"{"access_token":"renewed-access","refresh_token":"renewed-refresh","expires_in":3600}"#,
        );
        let dir = temp_dir("refresh");
        let path = dir.join("secrets.env");
        let mut expired = record_fixture(now_unix() - 5);
        expired.token_url = url;
        store_record(&path, "claude", &expired).unwrap();

        let token = access_token(&path, "claude").unwrap();
        let request = server.join().unwrap();
        assert_eq!(token, "renewed-access");
        // Refresh-запрос нёс СТАРЫЙ refresh-токен и client_id.
        assert!(request.contains("\"refresh_token\":\"stored-refresh\""));
        assert!(request.contains("\"grant_type\":\"refresh_token\""));
        // Реестр обновлён: новая пара сохранена.
        let reloaded = load_record(&path, "claude").unwrap().unwrap();
        assert_eq!(reloaded.access_token, "renewed-access");
        assert_eq!(reloaded.refresh_token, "renewed-refresh");
        assert!(reloaded.expires_at_unix > now_unix());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Провайдер не вернул новый refresh-токен — прежний остаётся в силе.
    #[test]
    fn refresh_without_new_refresh_token_keeps_old_one() {
        let (url, server) = serve_once(
            "200 OK",
            r#"{"access_token":"renewed-access","expires_in":3600}"#,
        );
        let dir = temp_dir("refresh-keep");
        let path = dir.join("secrets.env");
        let mut expired = record_fixture(now_unix() - 5);
        expired.token_url = url;
        store_record(&path, "openai", &expired).unwrap();
        assert_eq!(access_token(&path, "openai").unwrap(), "renewed-access");
        server.join().unwrap();
        let reloaded = load_record(&path, "openai").unwrap().unwrap();
        assert_eq!(reloaded.refresh_token, "stored-refresh");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn access_token_refresh_failure_surfaces_without_secrets() {
        let (url, server) = serve_once("400 Bad Request", r#"{"error":"invalid_grant"}"#);
        let dir = temp_dir("refresh-400");
        let path = dir.join("secrets.env");
        let mut expired = record_fixture(now_unix() - 5);
        expired.token_url = url;
        store_record(&path, "claude", &expired).unwrap();
        let err = access_token(&path, "claude").unwrap_err();
        server.join().unwrap();
        let message = err.to_string();
        assert!(!message.contains("stored-refresh"));
        assert!(!message.contains("stored-access"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn access_token_without_login_is_not_logged_in() {
        let dir = temp_dir("absent");
        let path = dir.join("secrets.env");
        assert!(matches!(
            access_token(&path, "claude"),
            Err(OAuthError::NotLoggedIn(_))
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    // --- logout / list ---

    #[test]
    fn logout_removes_only_own_entries() {
        let dir = temp_dir("logout");
        let path = dir.join("secrets.env");
        std::fs::write(&path, "ANTHROPIC_API_KEY=sk-ant\n").unwrap();
        store_record(&path, "claude", &record_fixture(now_unix() + 3600)).unwrap();
        store_record(&path, "openai", &record_fixture(now_unix() + 3600)).unwrap();
        assert!(remove_record(&path, "claude").unwrap());
        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(!contents.contains("BERIMOR_OAUTH_CLAUDE_"));
        assert!(contents.contains("BERIMOR_OAUTH_OPENAI_ACCESS_TOKEN="));
        assert!(contents.contains("ANTHROPIC_API_KEY=sk-ant"));
        // Повторный logout — false, не ошибка.
        assert!(!remove_record(&path, "claude").unwrap());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn bad_provider_names_are_rejected_before_touching_registry() {
        let dir = temp_dir("badname");
        let path = dir.join("secrets.env");
        assert!(matches!(
            store_record(&path, "../evil", &record_fixture(0)),
            Err(OAuthError::BadProviderName(_))
        ));
        assert!(!path.exists(), "файл не должен быть создан");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn profile_registry_covers_v1_providers() {
        assert_eq!(profile("claude").unwrap().token_body, TokenBodyFormat::Json);
        assert_eq!(profile("openai").unwrap().token_body, TokenBodyFormat::Form);
        assert!(matches!(
            profile("gemini"),
            Err(OAuthError::UnknownProvider(_))
        ));
    }
}
