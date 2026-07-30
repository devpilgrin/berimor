//! `berimor-capability` — deny-статика, jail, сетевой гейт, подтверждения, ACL плагинов.
//!
//! Источник: `docs/arch/security-model.md` §2 (L3), §3, §4. ROADMAP: F5, S1–S6.
//!
//! - `deny` (S1) — анализатор deny-статики: безусловный запрет пяти классов
//!   операций до выполнения.
//! - `jail` (S2) — jail файловой системы: канонизация путей, защита от
//!   symlink-обхода.
//! - `net_gate` (S3) — сетевой гейт: приватные адреса — через подтверждение.
//! - `confirm` (S4) — режимы подтверждений, декларация на инструмент,
//!   композитный гейт `StandardCapability` (deny-статика → режим).

use berimor_types::capability::{CapabilityDecision, ConfirmationMode, ProposedAction};

pub mod confirm;
pub mod deny;
pub mod jail;
pub mod net_gate;

/// Единая точка проверки перед любым мутирующим вызовом инструмента
/// (`process-engine.md` §4). `Deny` безусловен — обходить этот трейт
/// вызывающий код не может (инвариант I6).
pub trait CapabilityGate {
    fn check(&self, action: &ProposedAction, mode: ConfirmationMode) -> CapabilityDecision;
}

/// Манифест плагина — статический файл, который сам плагин переопределить
/// не может (`security-model.md` §4).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PluginManifest {
    pub name: String,
    pub allowed_events: Vec<String>,
    pub allowed_secrets: Vec<String>,
    pub capability_ceiling: Vec<String>,
}
