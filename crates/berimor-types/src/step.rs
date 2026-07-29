//! Граф процесса и типы шагов.
//!
//! Источник: `arch/process-engine.md` §2. Набор типов шагов намеренно
//! ограничен и закрыт — «выразительность приносится в жертву предсказуемости».
//! ROADMAP: P1, P2.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Ограниченный набор типов шагов. Ветвление (`Branch`) всегда работает на
/// уже валидированных полях состояния — модель никогда не выбирает ветку (I1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StepKind {
    Sequential,
    Parallel {
        branches: Vec<String>,
    },
    Loop {
        condition: String,
    },
    Branch {
        on: String,
        cases: BTreeMap<String, String>,
    },
    Tool {
        tool: String,
    },
    LlmStructured {
        contract: String,
        model_tier: crate::model::ModelTier,
    },
    CodeAct {
        contract: String,
    },
    AgentStep {
        max_turns: u32,
    },
    HumanGate {
        reason_template: String,
    },
    Checkpoint,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Step {
    pub id: String,
    pub kind: StepKind,
}

/// Детерминированные прерыватели процесса — `process-engine.md` §4,
/// расширено `cost_budget`/`latency_budget_ms` (ADR-0011).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessLimits {
    pub max_steps: u32,
    pub timeout_seconds: u64,
    pub token_budget: Option<u64>,
    pub cost_budget: Option<f64>,
    pub latency_budget_ms: Option<u64>,
}

/// Декларативная модель процесса. Версионируется; хэш версии пишется в
/// каждое событие. Инстанс фиксирует версию при создании и не меняет её
/// сам по себе — миграция только через явную операцию (ADR-0012).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Process {
    pub name: String,
    pub version: u32,
    pub steps: Vec<Step>,
    pub limits: ProcessLimits,
}

/// Декларативное описание изменения состояния. Патч не мутирует состояние
/// сам — применяет его только движок, атомарно (`process-engine.md` §3).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Patch {
    pub step_id: String,
    pub changes: serde_json::Value,
}
