//! Подкоманды `trace`/`eval` — только чтение журнала, без побочных
//! эффектов на реальный прогон (интеграция O1/O2 из `berimor-eval`).
//!
//! Источник: `docs/ROADMAP.md` §13 (Фаза 9) честно отмечает интеграцию в
//! CLI как вне тогдашнего scope — здесь она подключена (CLI-M3). O3
//! (`skill_health`)/O4 (`online_metrics`) сюда не входят: обоим нужен
//! агрегированный источник данных (использование навыка, список всех
//! инстансов), которого в CLI пока нет — `EventLog` не умеет перечислить
//! все инстансы журнала. Честный пробел, не забытая строка.

use crate::config::Config;
use crate::run::{build_executor_bundle, CliExecutor, RunError};
use berimor_context_engine::memory_builder::{FactsSource, MemoryContextBuilder};
use berimor_executors::{
    agent_step::AgentStepExecutor,
    codeact::{CodeActExecutor, WasmHost},
    structured_llm::StructuredLlm,
};
use berimor_process_engine::{engine, parser};
use berimor_storage::{SqliteEventLog, StorageError};
use berimor_types::event::ProcessInstanceId;
use serde_json::Value;
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum ObserveError {
    #[error(transparent)]
    Run(#[from] RunError),
    #[error("не удалось открыть журнал {path}: {reason}")]
    OpenStorage { path: PathBuf, reason: String },
    #[error("журнал: {0}")]
    Storage(#[from] StorageError),
    #[error("не удалось прочитать golden-набор {path}: {reason}")]
    ReadGoldenSet { path: PathBuf, reason: String },
    #[error("не удалось разобрать процесс golden-набора: {0}")]
    ParseProcess(String),
    #[error("сценарий {name}: некорректный JSON входа: {reason}")]
    BadScenario { name: String, reason: String },
}

/// `berimor trace <instance>` — человекочитаемая трассировка журнала
/// одного инстанса (O1). Неизвестный инстанс — пустой вывод, не ошибка
/// (то же соглашение, что у `berimor_eval::trace::trace`).
pub fn trace(config: &Config, instance: &str) -> Result<(), ObserveError> {
    let storage = open_storage(config)?;
    let id = ProcessInstanceId(instance.to_string());
    let entries = berimor_eval::trace::trace(&storage, &id)?;

    if entries.is_empty() {
        println!("[berimor] инстанс '{instance}' не найден или пуст");
        return Ok(());
    }
    for entry in entries {
        println!("[{:>6}] {:<24} {}", entry.seq.0, entry.kind, entry.summary);
    }
    Ok(())
}

/// `berimor eval <golden_dir>` — офлайн-прогон золотого набора (O2).
/// `golden_dir` содержит ровно один `process.yaml` и произвольное число
/// `<сценарий>.json` (вход процесса), имя файла без расширения — имя
/// сценария в отчёте.
pub fn eval(config: &Config, golden_dir: &Path) -> Result<(), ObserveError> {
    let process_path = golden_dir.join("process.yaml");
    let process_text =
        std::fs::read_to_string(&process_path).map_err(|err| ObserveError::ReadGoldenSet {
            path: process_path.clone(),
            reason: err.to_string(),
        })?;
    let process =
        parser::parse(&process_text).map_err(|err| ObserveError::ParseProcess(err.to_string()))?;

    // Эфемерный журнал (не config.storage_path!): id инстанса сценария —
    // "{процесс}::{сценарий}" (детерминированный, решает run_golden_set),
    // без проверки уникальности. Указать eval на реальный журнал прогонов
    // значило бы копить события повторных запусков под тем же id при
    // каждой перезапуске eval против одного config.toml — метрики (доля
    // веток/отказов) читались бы из объединённой истории всех прошлых
    // прогонов eval, а не только текущего (найдено независимым ревью
    // интеграции CLI-M1/M2/M3). Каждый вызов `eval` обязан начинать с
    // чистого журнала.
    let storage = SqliteEventLog::open_in_memory().map_err(|err| ObserveError::OpenStorage {
        path: PathBuf::from(":memory:"),
        reason: err.to_string(),
    })?;
    let bundle = build_executor_bundle(config)?;
    let providers = bundle.providers();
    // Находка 4.5 аудита: Session-слой eval искал по ЭФЕМЕРНОМУ журналу
    // стенда — находил события соседних сценариев того же прогона и
    // самоотсылку, но никогда реальные прошлые сессии. Разведено:
    // события сценариев — эфемерный журнал (чистые метрики, выше), а
    // Session-поиск — РЕАЛЬНЫЙ журнал прогонов (только чтение контекста;
    // отсутствие файла = пустая история, не ошибка).
    let real_journal = SqliteEventLog::open(&config.storage_path).ok();
    let episodic: &dyn berimor_storage::EpisodicSearch = match &real_journal {
        Some(journal) => journal,
        None => &storage,
    };
    // prompt-next-wave.md задача 1: та же поправка находки 4.5, что уже
    // применена к episodic выше — факты пишутся в РЕАЛЬНЫЙ журнал
    // (fact_extraction), эфемерный журнал сценария их никогда не увидит.
    let semantic_store: &dyn berimor_storage::SemanticStore = match &real_journal {
        Some(journal) => journal,
        None => &storage,
    };
    let facts_embed = crate::run::facts_embed_fn(config.memory.embeddings);
    let memory_context = MemoryContextBuilder {
        episodic,
        skills: &bundle.skills,
        session_search_limit: config.memory.session_search_limit,
        entity_graph: config
            .memory
            .entity_graph
            .then_some(&storage as &dyn berimor_storage::EntityGraphStore),
        facts: facts_embed.as_deref().map(|embed| FactsSource {
            store: semantic_store,
            embed,
            limit: config.memory.facts_search_limit,
        }),
        masker: Some(bundle.masker.as_ref()),
    };
    // Без телеметрии Mediation (on_attempt: None): у неё нет
    // фиксированного instance_id до вызова engine::instantiate ВНУТРИ
    // run_golden_set (id — "{процесс}::{сценарий}", решает сам стенд, не
    // вызывающий код). golden.rs документирует это как ограничение
    // исполнителя, не стенда: доля отказов сценария останется 0/0, если
    // исполнитель не журналирует Mediation сам — тот же честный пробел.
    let llm = StructuredLlm {
        pool: &bundle.pool,
        providers: &providers,
        context: &memory_context,
        on_attempt: None,
        secrets: bundle.masker.as_ref(),
    };
    let agent_step = AgentStepExecutor {
        pool: &bundle.pool,
        providers: &providers,
        context: &memory_context,
        on_attempt: None,
        gate: bundle.gate.as_ref(),
        mode: config.confirmation_mode,
        confirmer: bundle.confirmer.as_ref(),
        dispatch: bundle.dispatch.as_ref(),
        secrets: bundle.masker.as_ref(),
        on_tool_turn: None,
        on_provider_switch: None,
        tool_lines: crate::chat::tool_prompt_lines(config),
    };
    let wasm_host = WasmHost::new(
        bundle.dispatch.clone(),
        bundle.gate.clone(),
        config.confirmation_mode,
        bundle.confirmer.clone(),
        std::sync::Arc::clone(&bundle.masker),
    );
    let codeact = CodeActExecutor {
        pool: &bundle.pool,
        providers: &providers,
        context: &memory_context,
        on_attempt: None,
        wasm_host: &wasm_host,
        secrets: bundle.masker.as_ref(),
    };
    let executor = CliExecutor {
        gate: bundle.gate.as_ref(),
        mode: config.confirmation_mode,
        confirmer: bundle.confirmer.as_ref(),
        agent_step: &agent_step,
        codeact: &codeact,
        dispatch: bundle.dispatch.as_ref(),
        llm: &llm,
        latency_budget_ms: process.limits.latency_budget_ms,
        masker: bundle.masker.as_ref(),
    };

    let mut scenario_inputs: Vec<(String, Value)> = Vec::new();
    let dir_entries = std::fs::read_dir(golden_dir).map_err(|err| ObserveError::ReadGoldenSet {
        path: golden_dir.to_path_buf(),
        reason: err.to_string(),
    })?;
    for entry in dir_entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let name = path.file_stem().unwrap().to_string_lossy().to_string();
        let raw = std::fs::read_to_string(&path).map_err(|err| ObserveError::ReadGoldenSet {
            path: path.clone(),
            reason: err.to_string(),
        })?;
        let input: Value = serde_json::from_str(&raw).map_err(|err| ObserveError::BadScenario {
            name: name.clone(),
            reason: err.to_string(),
        })?;
        scenario_inputs.push((name, input));
    }
    scenario_inputs.sort_by(|a, b| a.0.cmp(&b.0));

    let scenarios: Vec<berimor_eval::golden::GoldenScenario> = scenario_inputs
        .into_iter()
        .map(|(name, input)| berimor_eval::golden::GoldenScenario {
            name,
            input,
            executor: &executor,
        })
        .collect();

    let report = berimor_eval::golden::run_golden_set(&storage, &process, &scenarios);

    println!(
        "[berimor] golden-набор: {} сценариев",
        report.scenarios.len()
    );
    for outcome in &report.scenarios {
        let status = match &outcome.result {
            Ok(engine::RunOutcome::Finished) => "finished".to_string(),
            Ok(engine::RunOutcome::AwaitingHuman { step_id, .. }) => {
                format!("awaiting_human({step_id})")
            }
            Err(err) => format!("error({err})"),
        };
        println!(
            "  {} — {status}, шагов достигнуто: {}",
            outcome.name,
            outcome.steps_reached.len()
        );
    }
    println!(
        "[berimor] доля веток: {:.2}, доля отказов Mediation: {:.2}",
        report.branch_coverage, report.failure_rate
    );

    Ok(())
}

fn open_storage(config: &Config) -> Result<SqliteEventLog, ObserveError> {
    SqliteEventLog::open(&config.storage_path).map_err(|err| ObserveError::OpenStorage {
        path: config.storage_path.clone(),
        reason: err.to_string(),
    })
}
