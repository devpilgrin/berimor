//! OTLP-экспорт журнала (волна B, 0.39.0): прогон процесса — это трейс.
//! Узел графа (StepApplied), вызов LLM (ModelUsage), human_gate и ход
//! инструмента свободного цикла становятся OTel-спанами; выгрузка —
//! OTLP/HTTP JSON (POST {endpoint}/v1/traces), формат принимают
//! коллекторы Jaeger, Grafana Tempo и Langfuse (отдельные экспортёры
//! под них не строятся — единый OTLP, см. ROADMAP §21).
//!
//! Детерминизм: traceId/spanId — хэши от id прогона и seq события,
//! повторный экспорт даёт те же идентификаторы (идемпотентно для
//! бэкенда). Иерархия: run (Instantiated → конец) → шаги (интервал до
//! своего StepApplied) → вложенные llm/gate/tool-спаны.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use berimor_types::event::{Event, EventKind};
use serde_json::{json, Value};

/// Спан в промежуточном виде (до сборки OTLP JSON).
#[derive(Debug, Clone)]
struct Span {
    name: String,
    span_id: String,
    parent: Option<String>,
    start_ns: u64,
    end_ns: u64,
    attrs: Vec<(String, Value)>,
    error: bool,
}

/// Детерминированный hex-id: 16 байт для traceId, 8 для spanId.
fn hex_id(seed: &str, bytes: usize) -> String {
    let mut out = String::with_capacity(bytes * 2);
    for chunk in 0..(bytes / 8) {
        let mut h = DefaultHasher::new();
        seed.hash(&mut h);
        chunk.hash(&mut h);
        out.push_str(&format!("{:016x}", h.finish()));
    }
    out.truncate(bytes * 2);
    out
}

fn ms_to_ns(ms: i64) -> u64 {
    (ms.max(0) as u64) * 1_000_000
}

fn attr(key: &str, value: Value) -> Value {
    let v = match value {
        Value::String(s) => json!({"stringValue": s}),
        Value::Number(n) if n.is_i64() || n.is_u64() => {
            json!({"intValue": n.to_string()})
        }
        Value::Number(n) => json!({"doubleValue": n.as_f64().unwrap_or(0.0)}),
        Value::Bool(b) => json!({"boolValue": b}),
        other => json!({"stringValue": other.to_string()}),
    };
    json!({"key": key, "value": v})
}

/// Построить спаны из событий журнала прогона.
fn build_spans(events: &[Event]) -> Vec<Span> {
    let mut spans: Vec<Span> = Vec::new();
    if events.is_empty() {
        return spans;
    }
    let run_seed = &events[0].process_instance.0;
    let root_id = hex_id(&format!("{run_seed}:run"), 8);
    let run_start = events[0].ts_ms;
    let run_end = events.last().map(|e| e.ts_ms).unwrap_or(run_start);

    // Корневой спан прогона.
    spans.push(Span {
        name: format!("process.run {}", events[0].process_instance.0),
        span_id: root_id.clone(),
        parent: None,
        start_ns: ms_to_ns(run_start),
        end_ns: ms_to_ns(run_end.max(run_start)),
        attrs: vec![
            ("berimor.instance".into(), json!(run_seed)),
            (
                "berimor.process_version".into(),
                json!(events[0].process_version),
            ),
        ],
        error: false,
    });

    // Шаг: интервал от предыдущего события до своего StepApplied;
    // вложенные спаны цепляются к «открытому» (последнему) шагу.
    let mut current_step: Option<(String, String)> = None; // (span_id, step_id)
    let mut gate_open: Option<(i64, String, String)> = None;

    for (i, event) in events.iter().enumerate() {
        let sid = || hex_id(&format!("{run_seed}:{}", event.seq.0 + i as u64 + 1), 8);
        let parent = current_step
            .as_ref()
            .map(|(id, _)| id.clone())
            .unwrap_or_else(|| root_id.clone());
        match &event.kind {
            EventKind::StepApplied { step_id }
            | EventKind::ParallelStepApplied {
                branch_step_id: step_id,
                ..
            } => {
                let span_id = sid();
                // Начало шага — предыдущее событие (вызовы модели этого
                // шага шли до StepApplied, в его интервале).
                let start = current_step
                    .as_ref()
                    .map(|_| event.ts_ms) // предыдущий шаг закрыт
                    .unwrap_or(event.ts_ms);
                spans.push(Span {
                    name: format!("step {step_id}"),
                    span_id: span_id.clone(),
                    parent: Some(root_id.clone()),
                    start_ns: ms_to_ns(start),
                    end_ns: ms_to_ns(event.ts_ms),
                    attrs: vec![("berimor.step_id".into(), json!(step_id))],
                    error: false,
                });
                current_step = Some((span_id, step_id.clone()));
            }
            EventKind::ModelUsage {
                step_id,
                provider,
                model_id,
                prompt_tokens,
                completion_tokens,
                latency_ms,
            } => {
                let end = event.ts_ms;
                let start = end.saturating_sub(*latency_ms as i64);
                spans.push(Span {
                    name: format!("llm {provider}/{model_id}"),
                    span_id: sid(),
                    parent: Some(parent),
                    start_ns: ms_to_ns(start),
                    end_ns: ms_to_ns(end),
                    attrs: vec![
                        ("llm.provider".into(), json!(provider)),
                        ("llm.model".into(), json!(model_id)),
                        ("llm.usage.prompt_tokens".into(), json!(prompt_tokens)),
                        (
                            "llm.usage.completion_tokens".into(),
                            json!(completion_tokens),
                        ),
                        ("berimor.step_id".into(), json!(step_id)),
                    ],
                    error: false,
                });
            }
            EventKind::HumanGateOpened { reason } => {
                gate_open = Some((event.ts_ms, sid(), reason.clone()));
            }
            EventKind::HumanGateResolved | EventKind::HumanGateTimedOut { .. } => {
                let (opened, span_id, reason) =
                    gate_open
                        .take()
                        .unwrap_or((event.ts_ms, sid(), String::new()));
                let timed_out = matches!(event.kind, EventKind::HumanGateTimedOut { .. });
                spans.push(Span {
                    name: "human_gate".to_string(),
                    span_id,
                    parent: Some(parent),
                    start_ns: ms_to_ns(opened),
                    end_ns: ms_to_ns(event.ts_ms),
                    attrs: vec![
                        ("berimor.gate.reason".into(), json!(reason)),
                        ("berimor.gate.timed_out".into(), json!(timed_out)),
                    ],
                    error: timed_out,
                });
            }
            EventKind::AgentToolTurn { tool, ok, .. } => {
                spans.push(Span {
                    name: format!("tool {tool}"),
                    span_id: sid(),
                    parent: Some(parent),
                    start_ns: ms_to_ns(event.ts_ms),
                    end_ns: ms_to_ns(event.ts_ms),
                    attrs: vec![
                        ("berimor.tool".into(), json!(tool)),
                        ("berimor.tool.ok".into(), json!(ok)),
                    ],
                    error: !ok,
                });
            }
            _ => {}
        }
    }
    spans
}

/// Собрать OTLP/HTTP JSON payload.
fn to_otlp_json(instance: &str, spans: &[Span]) -> Value {
    let trace_id = hex_id(instance, 16);
    let spans_json: Vec<Value> = spans
        .iter()
        .map(|s| {
            let mut span = json!({
                "traceId": trace_id,
                "spanId": s.span_id,
                "name": s.name,
                "kind": 1,
                "startTimeUnixNano": s.start_ns.to_string(),
                "endTimeUnixNano": s.end_ns.to_string(),
                "attributes": s.attrs.iter().map(|(k, v)| attr(k, v.clone())).collect::<Vec<_>>(),
                "status": {"code": if s.error { 2 } else { 1 }},
            });
            if let Some(parent) = &s.parent {
                span["parentSpanId"] = json!(parent);
            }
            span
        })
        .collect();
    json!({
        "resourceSpans": [{
            "resource": {
                "attributes": [
                    attr("service.name", json!("berimor")),
                    attr("berimor.run", json!(instance)),
                ]
            },
            "scopeSpans": [{
                "scope": {"name": "berimor.journal"},
                "spans": spans_json,
            }]
        }]
    })
}

/// `berimor otlp <instance> --endpoint <url>`: экспорт прогона в
/// OTLP/HTTP JSON (Jaeger/Tempo — http://localhost:4318; Langfuse —
/// свой otel-эндпоинт; заголовки авторизации — флагом).
pub fn export(
    config: &crate::config::Config,
    instance: &str,
    endpoint: &str,
    headers: &[(String, String)],
) -> Result<(), String> {
    use berimor_storage::EventLog;
    let storage = berimor_storage::SqliteEventLog::open(&config.storage_path)
        .map_err(|err| format!("журнал: {err}"))?;
    let events = storage
        .replay(&berimor_types::event::ProcessInstanceId(
            instance.to_string(),
        ))
        .map_err(|err| format!("журнал: {err}"))?;
    if events.is_empty() {
        return Err(format!("инстанс '{instance}' не найден или пуст"));
    }
    let spans = build_spans(&events);
    let payload = to_otlp_json(instance, &spans);
    let url = format!("{}/v1/traces", endpoint.trim_end_matches('/'));
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|err| format!("http-клиент: {err}"))?;
    let mut request = client.post(&url).json(&payload);
    for (name, value) in headers {
        request = request.header(name, value);
    }
    let response = request
        .send()
        .map_err(|err| format!("отправка в {url}: {err}"))?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().unwrap_or_default();
        return Err(format!(
            "коллектор ответил {status}: {}",
            &body[..body.len().min(300)]
        ));
    }
    println!(
        "[berimor] экспортировано: {} спанов прогона '{instance}' → {url}",
        spans.len()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use berimor_types::event::ProcessInstanceId;

    fn event(seq: u64, ts: i64, kind: EventKind) -> Event {
        let mut e = Event::new(ProcessInstanceId("run-x".into()), 1, kind, Value::Null);
        e.seq = berimor_types::event::EventSeq(seq);
        e.ts_ms = ts;
        e
    }

    #[test]
    fn spans_cover_steps_llm_and_gate() {
        let events = vec![
            event(1, 1000, EventKind::Instantiated),
            event(
                2,
                2000,
                EventKind::ModelUsage {
                    step_id: Some("classify".into()),
                    provider: "kimi".into(),
                    model_id: "k3".into(),
                    prompt_tokens: 100,
                    completion_tokens: 50,
                    latency_ms: 900,
                },
            ),
            event(
                3,
                2100,
                EventKind::StepApplied {
                    step_id: "classify".into(),
                },
            ),
            event(
                4,
                2200,
                EventKind::HumanGateOpened {
                    reason: "высокий риск".into(),
                },
            ),
            event(5, 5000, EventKind::HumanGateResolved),
        ];
        let spans = build_spans(&events);
        let names: Vec<&str> = spans.iter().map(|s| s.name.as_str()).collect();
        assert!(names.iter().any(|n| n.starts_with("process.run")));
        assert!(names.contains(&"step classify"));
        assert!(names.contains(&"llm kimi/k3"));
        assert!(names.contains(&"human_gate"));
        // llm-спан: 900 мс латентности → start = 2000-900 = 1100 мс.
        let llm = spans.iter().find(|s| s.name == "llm kimi/k3").unwrap();
        assert_eq!(llm.start_ns, 1_100_000_000);
        // human_gate — интервал 2200..5000.
        let gate = spans.iter().find(|s| s.name == "human_gate").unwrap();
        assert_eq!(gate.end_ns - gate.start_ns, 2_800_000_000);
    }

    #[test]
    fn otlp_json_shape_and_deterministic_ids() {
        let events = vec![
            event(1, 1000, EventKind::Instantiated),
            event(
                2,
                1500,
                EventKind::StepApplied {
                    step_id: "a".into(),
                },
            ),
        ];
        let spans = build_spans(&events);
        let payload = to_otlp_json("run-x", &spans);
        let spans_json = &payload["resourceSpans"][0]["scopeSpans"][0]["spans"];
        assert_eq!(spans_json.as_array().unwrap().len(), 2);
        let tid = spans_json[0]["traceId"].as_str().unwrap();
        assert_eq!(tid.len(), 32);
        // Повторная сборка — те же идентификаторы.
        let payload2 = to_otlp_json("run-x", &build_spans(&events));
        assert_eq!(payload, payload2);
        // Шаг — ребёнок корня.
        assert_eq!(
            spans_json[1]["parentSpanId"].as_str().unwrap(),
            spans_json[0]["spanId"].as_str().unwrap()
        );
    }
}
