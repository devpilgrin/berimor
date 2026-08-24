//! Обработчик `POST /webhooks/github` в serve (волна F, 0.43.0):
//! HMAC-подпись вместо bearer → маршрутизация события → 202 сразу,
//! процесс — в фоновом потоке (у GitHub таймаут вебхука ~10 c),
//! итог — комментарием в issue/PR через installation token.

use std::net::TcpStream;

use serde_json::json;

use crate::config::{Config, GithubAppConfig};
use crate::ghapp::{self, WebhookAction};
use crate::serve::{write_json, Request};

/// Точка входа из serve::handle_connection (до bearer-проверки).
pub fn handle_github_webhook(stream: &mut TcpStream, config: &Config, request: &Request) {
    let Some(app) = config.github_app.as_ref() else {
        return write_json(
            stream,
            404,
            &json!({"error": "github_app не настроен ([github_app] в config.toml)"}),
        );
    };
    let secret = std::env::var(&app.webhook_secret_env).unwrap_or_default();
    if secret.is_empty() {
        return write_json(
            stream,
            500,
            &json!({"error": format!("переменная {} пуста", app.webhook_secret_env)}),
        );
    }
    let signature = request.x_hub_signature.clone().unwrap_or_default();
    if !ghapp::verify_signature(&secret, &request.body, &signature) {
        return write_json(stream, 401, &json!({"error": "подпись вебхука не сошлась"}));
    }
    let trigger = app
        .trigger
        .clone()
        .unwrap_or_else(|| "/berimor".to_string());
    let event = request.x_github_event.clone().unwrap_or_default();
    match ghapp::route_event(&event, &request.body, &trigger) {
        WebhookAction::Ignore => {
            write_json(stream, 202, &json!({"status": "ignored"}));
        }
        WebhookAction::RunFromComment {
            installation_id,
            repo_full_name,
            issue_number,
            comment_body,
        } => {
            // 202 немедленно: процесс может идти минуты, GitHub ждать не будет.
            write_json(stream, 202, &json!({"status": "accepted"}));
            let config = config.clone();
            let app = app.clone();
            std::thread::spawn(move || {
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run_and_reply(
                        &config,
                        &app,
                        installation_id,
                        &repo_full_name,
                        issue_number,
                        &comment_body,
                    )
                }));
                if let Err(panic) = outcome {
                    eprintln!("[berimor] github_app: обработчик упал: {panic:?}");
                }
            });
        }
    }
}

/// Фон: JWT → installation token → процесс → комментарий с итогом.
fn run_and_reply(
    config: &Config,
    app: &GithubAppConfig,
    installation_id: u64,
    repo_full_name: &str,
    issue_number: u64,
    comment_body: &str,
) {
    let api_base = app
        .api_base
        .clone()
        .unwrap_or_else(|| "https://api.github.com".to_string());
    let result = (|| -> Result<String, crate::run::RunError> {
        let pem = std::fs::read_to_string(&app.private_key_path).map_err(|err| {
            crate::run::RunError::Gate(format!(
                "github_app: ключ {}: {err}",
                app.private_key_path.display()
            ))
        })?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let jwt = ghapp::mint_app_jwt(app.app_id, &pem, now)?;
        let token = ghapp::installation_token(&api_base, &jwt, installation_id)?;
        // Процесс-обработчик: вход — контекст вызова.
        let input = json!({
            "repo": repo_full_name,
            "issue": issue_number,
            "comment": comment_body,
        })
        .to_string();
        let summary = match crate::run::run(config, &app.process, &None, &Some(input), true) {
            Ok(()) => "процесс завершился успешно".to_string(),
            Err(crate::run::RunError::HumanDeclined) => {
                "процесс остановлен на human_gate: в CI подтвердить некому".to_string()
            }
            Err(err) => format!("процесс завершился с ошибкой: {err}"),
        };
        let text = format!("**berimor** · `{summary}`\n\n_журнал прогона — на стороне сервиса_");
        ghapp::post_comment(&api_base, &token, repo_full_name, issue_number, &text)?;
        Ok(text)
    })();
    if let Err(err) = result {
        eprintln!("[berimor] github_app: {err}");
    }
}
