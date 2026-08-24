//! berimor как GitHub App (волна F, 0.43.0): эндпоинт вебхуков в `serve`.
//! GitHub шлёт события (issue_comment / pull_request) → верификация
//! HMAC-SHA256 по webhook secret → installation access token (JWT RS256)
//! → запуск процесса в неинтерактивном режиме → ответ комментарием
//! в issue/PR. Аутентификация эндпоинта — подпись GitHub, не bearer-токен
//! serve (GitHub его не знает).
//!
//! Конфиг `[github_app]` в config.toml: app_id, private_key_path (PEM,
//! RSA), webhook_secret, process (YAML процесса-обработчика), trigger
//! (метка в комментарии, дефолт "/berimor").

use hmac::{Hmac, Mac};
use serde::Deserialize;
use sha2::Sha256;

use crate::run::RunError;

/// Событие вебхука, которое мы обрабатываем.
#[derive(Debug)]
pub enum WebhookAction {
    /// Запустить процесс по триггеру в комментарии issue/PR.
    RunFromComment {
        installation_id: u64,
        repo_full_name: String,
        issue_number: u64,
        comment_body: String,
    },
    /// Игнорируем (не наше событие/нет триггера).
    Ignore,
}

/// Верификация X-Hub-Signature-256: hex(HMAC-SHA256(secret, body)).
pub fn verify_signature(secret: &str, body: &[u8], signature_header: &str) -> bool {
    let Some(hex_sig) = signature_header.strip_prefix("sha256=") else {
        return false;
    };
    let Ok(expected) = hex::decode(hex_sig) else {
        return false;
    };
    let mut mac = match Hmac::<Sha256>::new_from_slice(secret.as_bytes()) {
        Ok(mac) => mac,
        Err(_) => return false,
    };
    mac.update(body);
    mac.verify_slice(&expected).is_ok()
}

/// Разбор вебхука: тип события + тело → действие.
pub fn route_event(event: &str, body: &[u8], trigger: &str) -> WebhookAction {
    if event != "issue_comment" {
        return WebhookAction::Ignore;
    }
    #[derive(Deserialize)]
    struct Hook {
        action: String,
        installation: Installation,
        repository: Repo,
        issue: Issue,
        comment: Comment,
    }
    #[derive(Deserialize)]
    struct Installation {
        id: u64,
    }
    #[derive(Deserialize)]
    struct Repo {
        full_name: String,
    }
    #[derive(Deserialize)]
    struct Issue {
        number: u64,
    }
    #[derive(Deserialize)]
    struct Comment {
        body: String,
    }
    let Ok(hook) = serde_json::from_slice::<Hook>(body) else {
        return WebhookAction::Ignore;
    };
    if hook.action != "created" || !hook.comment.body.contains(trigger) {
        return WebhookAction::Ignore;
    }
    WebhookAction::RunFromComment {
        installation_id: hook.installation.id,
        repo_full_name: hook.repository.full_name,
        issue_number: hook.issue.number,
        comment_body: hook.comment.body,
    }
}

/// JWT приложения (RS256, iss=app_id, жизнь 9 минут — GitHub требует ≤10).
pub fn mint_app_jwt(app_id: u64, private_key_pem: &str, now_secs: u64) -> Result<String, RunError> {
    #[derive(serde::Serialize)]
    struct Claims {
        iat: u64,
        exp: u64,
        iss: u64,
    }
    let claims = Claims {
        iat: now_secs.saturating_sub(30),
        exp: now_secs + 9 * 60,
        iss: app_id,
    };
    let key = jsonwebtoken::EncodingKey::from_rsa_pem(private_key_pem.as_bytes())
        .map_err(|err| RunError::Gate(format!("github_app: закрытый ключ не читается: {err}")))?;
    jsonwebtoken::encode(
        &jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256),
        &claims,
        &key,
    )
    .map_err(|err| RunError::Gate(format!("github_app: не удалось подписать JWT: {err}")))
}

/// Обмен JWT на installation access token через api.github.com.
pub fn installation_token(
    api_base: &str,
    jwt: &str,
    installation_id: u64,
) -> Result<String, RunError> {
    let url = format!("{api_base}/app/installations/{installation_id}/access_tokens");
    let mut response = ureq::post(&url)
        .header("Authorization", &format!("Bearer {jwt}"))
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "berimor-ghapp")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .send_empty()
        .map_err(|err| RunError::Gate(format!("github_app: обмен токена: {err}")))?;
    let body: serde_json::Value = response
        .body_mut()
        .read_json()
        .map_err(|err| RunError::Gate(format!("github_app: ответ токена не JSON: {err}")))?;
    body.get("token")
        .and_then(|token| token.as_str())
        .map(str::to_string)
        .ok_or_else(|| RunError::Gate("github_app: в ответе нет token".into()))
}

/// Комментарий в issue/PR от имени установки.
pub fn post_comment(
    api_base: &str,
    token: &str,
    repo_full_name: &str,
    issue_number: u64,
    text: &str,
) -> Result<(), RunError> {
    let url = format!("{api_base}/repos/{repo_full_name}/issues/{issue_number}/comments");
    ureq::post(&url)
        .header("Authorization", &format!("Bearer {token}"))
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "berimor-ghapp")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .send_json(serde_json::json!({ "body": text }))
        .map_err(|err| RunError::Gate(format!("github_app: публикация комментария: {err}")))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read as _;

    fn sign(secret: &str, body: &[u8]) -> String {
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("hmac");
        mac.update(body);
        format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
    }

    #[test]
    fn signature_roundtrip_accepts_and_rejects() {
        let body = br#"{"zen":"ok"}"#;
        let good = sign("s3cret", body);
        assert!(verify_signature("s3cret", body, &good));
        assert!(!verify_signature("wrong", body, &good));
        assert!(!verify_signature("s3cret", body, "sha256=zz"));
        assert!(!verify_signature("s3cret", body, "md5=00"));
    }

    #[test]
    fn route_event_ignores_non_trigger_and_picks_trigger() {
        let body = serde_json::json!({
            "action": "created",
            "installation": {"id": 42},
            "repository": {"full_name": "devpilgrin/berimor"},
            "issue": {"number": 7},
            "comment": {"body": "посмотри пожалуйста\n/berimor review"}
        })
        .to_string();
        match route_event("issue_comment", body.as_bytes(), "/berimor") {
            WebhookAction::RunFromComment {
                installation_id,
                repo_full_name,
                issue_number,
                ..
            } => {
                assert_eq!(installation_id, 42);
                assert_eq!(repo_full_name, "devpilgrin/berimor");
                assert_eq!(issue_number, 7);
            }
            WebhookAction::Ignore => panic!("триггер должен был сработать"),
        }
        // другое событие / другой action / без метки — мимо
        assert!(matches!(
            route_event("pull_request", body.as_bytes(), "/berimor"),
            WebhookAction::Ignore
        ));
        let edited = body.replace("\"created\"", "\"edited\"");
        assert!(matches!(
            route_event("issue_comment", edited.as_bytes(), "/berimor"),
            WebhookAction::Ignore
        ));
        let plain = body.replace("/berimor review", "просто текст");
        assert!(matches!(
            route_event("issue_comment", plain.as_bytes(), "/berimor"),
            WebhookAction::Ignore
        ));
    }

    #[test]
    fn jwt_mints_rs256_with_app_id() {
        // Сгенерированный тестовый RSA-ключ (2048) — только для тестов.
        let pem = include_str!("../tests/fixtures/ghapp_test_rsa.pem");
        let jwt = mint_app_jwt(12345, pem, 1_800_000_000).expect("jwt");
        let parts: Vec<&str> = jwt.split('.').collect();
        assert_eq!(parts.len(), 3);
        let header: serde_json::Value =
            serde_json::from_slice(&decode_b64url(parts[0])).expect("header");
        assert_eq!(header["alg"], "RS256");
        let claims: serde_json::Value =
            serde_json::from_slice(&decode_b64url(parts[1])).expect("claims");
        assert_eq!(claims["iss"], 12345);
        assert!(claims["exp"].as_u64().expect("exp") > claims["iat"].as_u64().expect("iat"));
    }

    fn decode_b64url(part: &str) -> Vec<u8> {
        use base64::Engine;
        base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(part)
            .expect("b64url")
    }

    /// Живой round-trip против локального стаба api.github.com:
    /// обмен токена + публикация комментария.
    #[test]
    fn token_exchange_and_comment_against_stub() {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let handle = std::thread::spawn(move || {
            // 1: access_tokens → JSON с token; 2: comments → 201
            for expected in ["access_tokens", "comments"] {
                let (mut stream, _) = listener.accept().expect("accept");
                let mut buf = [0u8; 8192];
                let n = stream.read(&mut buf).unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..n]).to_string();
                assert!(request.contains(expected), "стаб ждал {expected}");
                assert!(
                    request.contains("Bearer test-jwt") || request.contains("Bearer inst-token")
                );
                let body = if expected == "access_tokens" {
                    "{\"token\":\"inst-token\"}"
                } else {
                    "{\"id\":1}"
                };
                let response = format!(
                    "HTTP/1.1 201 Created\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                    body.len(),
                    body
                );
                use std::io::Write;
                stream.write_all(response.as_bytes()).expect("write");
            }
        });
        let api = format!("http://127.0.0.1:{port}");
        let token = installation_token(&api, "test-jwt", 42).expect("token");
        assert_eq!(token, "inst-token");
        post_comment(&api, &token, "a/b", 7, "готово").expect("comment");
        handle.join().expect("join");
    }
}
