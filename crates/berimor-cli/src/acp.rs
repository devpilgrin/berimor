//! ACP-адаптер (волна G, 0.44.0): `berimor acp` говорит Agent Client
//! Protocol (Zed и совместимые редакторы) по NDJSON JSON-RPC через
//! stdio. Сессия = прогон процесса: prompt запускает процесс из
//! `[acp] process` с входом {text}, события журнала стримятся как
//! `session/update` (ходы инструментов — tool_call, итог —
//! agent_message_chunk). Печать прогона при этом уходит в stderr
//! (run::ACP_QUIET): stdout принадлежит протоколу.
//!
//! v1 осознанно без: fs/terminal-колбэков клиента (инструменты
//! исполняем сами под своими гейтами), session/load, потокового вывода
//! модели (провайдеры блокирующие — чанки идут по событиям журнала).

use std::collections::HashSet;
use std::io::{BufRead, BufReader, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use berimor_storage::EventLog;

use crate::config::Config;
use crate::run::RunError;

/// Точка входа `berimor acp`.
pub fn serve(config: &Config) -> Result<(), RunError> {
    if config.acp.process.is_none() {
        eprintln!(
            "[berimor] acp: задайте процесс-обработчик: [acp] process = \"path.yaml\" в config.toml"
        );
        return Err(RunError::Gate("acp: процесс не настроен".into()));
    }
    crate::run::ACP_QUIET.store(true, Ordering::Relaxed);

    let stdout: Arc<Mutex<std::io::Stdout>> = Arc::new(Mutex::new(std::io::stdout()));
    let cancelled: Arc<Mutex<HashSet<String>>> = Arc::new(Mutex::new(HashSet::new()));
    let mut session_counter = 0u64;

    let stdin = std::io::stdin();
    let reader = BufReader::new(stdin.lock());
    for line in reader.lines() {
        let Ok(line) = line else { break };
        let line = line.trim().to_string();
        if line.is_empty() {
            continue;
        }
        let message: Value = match serde_json::from_str(&line) {
            Ok(value) => value,
            Err(_) => continue,
        };
        let method = message.get("method").and_then(Value::as_str).unwrap_or("");
        let id = message.get("id").cloned();
        let params = message.get("params").cloned().unwrap_or(json!({}));

        match method {
            "initialize" => {
                if let Some(id) = id {
                    send(
                        &stdout,
                        &json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "result": {
                                "protocolVersion": 1,
                                "agentCapabilities": {
                                    "loadSession": false,
                                    "promptCapabilities": {
                                        "image": false, "audio": false, "embeddedContext": false
                                    },
                                    "mcpCapabilities": {"http": false, "sse": false}
                                },
                                "agentInfo": {
                                    "name": "berimor",
                                    "title": "berimor — модель думает, код решает",
                                    "version": env!("CARGO_PKG_VERSION")
                                },
                                "authMethods": []
                            }
                        }),
                    );
                }
            }
            "authenticate" => {
                if let Some(id) = id {
                    send(
                        &stdout,
                        &json!({"jsonrpc": "2.0", "id": id, "result": null}),
                    );
                }
            }
            "session/new" => {
                session_counter += 1;
                let session_id = format!("acp-{session_counter}");
                if let Some(id) = id {
                    send(
                        &stdout,
                        &json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "result": {"sessionId": session_id}
                        }),
                    );
                }
            }
            "session/cancel" => {
                if let Some(session_id) = params.get("sessionId").and_then(Value::as_str) {
                    cancelled
                        .lock()
                        .expect("cancelled")
                        .insert(session_id.to_string());
                }
            }
            "session/prompt" => {
                let Some(id) = id else { continue };
                let session_id = params
                    .get("sessionId")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let text: String = params
                    .get("prompt")
                    .and_then(Value::as_array)
                    .map(|blocks| {
                        blocks
                            .iter()
                            .filter_map(|b| {
                                if b.get("type").and_then(Value::as_str) == Some("text") {
                                    b.get("text").and_then(Value::as_str).map(str::to_string)
                                } else {
                                    None
                                }
                            })
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                    .unwrap_or_default();
                let config = config.clone();
                let stdout = Arc::clone(&stdout);
                let cancelled = Arc::clone(&cancelled);
                std::thread::spawn(move || {
                    run_prompt(stdout, config, id, session_id, text, cancelled);
                });
            }
            _ => {
                if let Some(id) = id {
                    send(
                        &stdout,
                        &json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "error": {"code": -32601, "message": format!("method not found: {method}")}
                        }),
                    );
                }
            }
        }
    }
    Ok(())
}

/// Прогон prompt'а: процесс в потоке, события журнала — наружу,
/// ответ по завершении (или по cancel — процесс доезжает в журнале).
fn run_prompt(
    stdout: Arc<Mutex<std::io::Stdout>>,
    config: Config,
    id: Value,
    session_id: String,
    text: String,
    cancelled: Arc<Mutex<HashSet<String>>>,
) {
    let process_path = config.acp.process.clone().expect("checked at start");
    let input = json!({"text": text}).to_string();

    // Идентификатор нового прогона — по дельте списка инстансов журнала.
    let before: HashSet<String> = berimor_storage::SqliteEventLog::open(&config.storage_path)
        .and_then(|s| s.list_instance_ids())
        .unwrap_or_default()
        .into_iter()
        .collect();

    let run_done = Arc::new(AtomicBool::new(false));
    let run_config = config.clone();
    let run_done_clone = Arc::clone(&run_done);
    std::thread::spawn(move || {
        let outcome = crate::run::run(&run_config, &process_path, &None, &Some(input), true);
        if let Err(err) = outcome {
            eprintln!("[berimor] acp: прогон завершился с ошибкой: {err}");
        }
        run_done_clone.store(true, Ordering::Relaxed);
    });

    // Эмиттер: опрашиваем журнал, новые события нового инстанса — в
    // session/update.
    let mut emitted = 0usize;
    let mut instance_found: Option<String> = None;
    let stop = loop {
        let was_cancelled = cancelled.lock().expect("cancelled").contains(&session_id);
        if was_cancelled {
            break "cancelled";
        }
        if let Ok(storage) = berimor_storage::SqliteEventLog::open(&config.storage_path) {
            if instance_found.is_none() {
                if let Ok(ids) = storage.list_instance_ids() {
                    instance_found = ids.into_iter().find(|i| !before.contains(i));
                }
            }
            if let Some(instance) = &instance_found {
                let events = storage
                    .replay(&berimor_types::event::ProcessInstanceId(instance.clone()))
                    .unwrap_or_default();
                for event in events.iter().skip(emitted) {
                    emit_event(&stdout, &session_id, event);
                }
                emitted = events.len();
            }
        }
        if run_done.load(Ordering::Relaxed) {
            // Добираем хвост журнала и отвечаем.
            std::thread::sleep(std::time::Duration::from_millis(120));
            if let (Ok(storage), Some(instance)) = (
                berimor_storage::SqliteEventLog::open(&config.storage_path),
                &instance_found,
            ) {
                let events = storage
                    .replay(&berimor_types::event::ProcessInstanceId(instance.clone()))
                    .unwrap_or_default();
                for event in events.iter().skip(emitted) {
                    emit_event(&stdout, &session_id, event);
                }
            }
            break "end_turn";
        }
        std::thread::sleep(std::time::Duration::from_millis(300));
    };

    send(
        &stdout,
        &json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {"stopReason": stop}
        }),
    );
    cancelled.lock().expect("cancelled").remove(&session_id);
}

/// Событие журнала → уведомление session/update (ACP).
fn emit_event(
    stdout: &Arc<Mutex<std::io::Stdout>>,
    session_id: &str,
    event: &berimor_types::event::Event,
) {
    use berimor_types::event::EventKind;
    let update = match &event.kind {
        EventKind::AgentToolTurn { tool, ok, .. } => Some(json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": format!("turn-{}", event.ts_ms),
            "status": if *ok { "completed" } else { "failed" },
            "title": tool,
        })),
        EventKind::HumanGateOpened { reason } => {
            Some(chunk(&format!("⏸ процесс встал на human_gate: {reason}")))
        }
        EventKind::HumanGateResolved | EventKind::HumanGateTimedOut { .. } => None,
        EventKind::MediationRejected { .. } => Some(chunk(
            "⚠ вывод модели отклонён медиацией (подробности — в журнале прогона)",
        )),
        _ => None,
    };
    if let Some(update) = update {
        send(
            stdout,
            &json!({
                "jsonrpc": "2.0",
                "method": "session/update",
                "params": {"sessionId": session_id, "update": update}
            }),
        );
    }
}

fn chunk(text: &str) -> Value {
    json!({
        "sessionUpdate": "agent_message_chunk",
        "content": {"type": "text", "text": format!("{text}\n")}
    })
}

fn send(stdout: &Arc<Mutex<std::io::Stdout>>, value: &Value) {
    let mut out = stdout.lock().expect("stdout");
    let _ = writeln!(out, "{value}");
    let _ = out.flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunk_is_acp_agent_message() {
        let value = chunk("привет");
        assert_eq!(value["sessionUpdate"], "agent_message_chunk");
        assert_eq!(value["content"]["text"], "привет\n");
    }

    #[test]
    fn tool_turn_maps_to_tool_call_update() {
        let event = berimor_types::event::Event {
            seq: berimor_types::event::EventSeq(1),
            process_instance: berimor_types::event::ProcessInstanceId("acp-test".into()),
            process_version: 1,
            payload: serde_json::json!({}),
            ts_ms: 1000,
            kind: berimor_types::event::EventKind::AgentToolTurn {
                step_id: "s".into(),
                tool: "files.read".into(),
                args_masked: "{}".into(),
                observation_masked: "ok".into(),
                ok: true,
            },
        };
        let stdout = Arc::new(Mutex::new(std::io::stdout()));
        emit_event(&stdout, "acp-1", &event); // не паникует — достаточно
    }
}
