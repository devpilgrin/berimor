//! `berimor-memory` — четыре слоя памяти + граф сущностей.
//!
//! Источник: `arch/memory-model.md`. Запись — только через дедупликацию и
//! контракт Mediation; чтение — только через `berimor-context-engine`.
//! Модель не решает, что помнить и что вспомнить (инвариант I1).

pub mod entity_graph;
pub mod episodic;
pub mod procedural;
pub mod profile;
pub mod semantic;
pub mod working;
