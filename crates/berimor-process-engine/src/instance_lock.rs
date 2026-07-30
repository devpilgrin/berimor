//! Гарантия «один писатель на инстанс» (P5, `process-engine.md` §4).
//!
//! Заимствование Rust уже даёт это внутри одного вызывающего: `&mut
//! ProcessInstance` не бывает двух одновременно, `SqliteEventLog` сам
//! сериализует запись в журнал одним `Mutex<Connection>`. Пробел не
//! здесь — а когда ДВА НЕЗАВИСИМЫХ вызывающих (два актора Фазы 7, два
//! процесса CLI) каждый восстанавливает СВОЙ `ProcessInstance` для
//! одного и того же `ProcessInstanceId` из журнала независимо и оба
//! вызывают [`crate::engine::run`] — тогда оба решают «следующий шаг» по
//! своей копии `state` и оба журналируют события, ничего не зная друг о
//! друге. `InstanceWriteLocks` — RAII-лиз на `ProcessInstanceId`,
//! который вызывающий код обязан взять ДО `run`/`recover`+`run`, чтобы
//! это исключить; сам движок лиз не берёт и не проверяет — навязывать
//! использование здесь нечем (I5: ядро не диктует вызывающему, как ему
//! устроена конкурентность), интеграция с конкретным вызывающим кодом
//! (CLI, диспетчер акторов A3) — не эта задача.

use berimor_types::event::ProcessInstanceId;
use std::collections::HashSet;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("инстанс {0:?} уже продвигается другим вызывающим")]
pub struct InstanceLockedError(pub ProcessInstanceId);

/// Реестр открытых лизов — дешёвый `Clone` (общее состояние за `Arc`), так
/// что несколько независимых вызывающих (акторов) держат ссылки на ОДИН
/// и тот же реестр и видят лизы друг друга.
#[derive(Clone, Default)]
pub struct InstanceWriteLocks {
    held: Arc<Mutex<HashSet<ProcessInstanceId>>>,
}

/// Лиз на один инстанс — снимается при `Drop`, не требует явного вызова
/// «освободить»: паника/ранний `return` вызывающего кода не оставляет
/// инстанс залоченным навсегда.
#[derive(Debug)]
pub struct InstanceLease {
    held: Arc<Mutex<HashSet<ProcessInstanceId>>>,
    id: ProcessInstanceId,
}

impl InstanceWriteLocks {
    pub fn new() -> Self {
        Self::default()
    }

    /// Берёт лиз на `id`, если он ещё не занят. Не блокирует и не ждёт —
    /// «один писатель» здесь про отказ второму, а не про очередь: тот же
    /// код-детерминизм, что и у остальных решений движка (I1) — вызывающий
    /// код сам решает, что делать с отказом (повторить позже, эскалировать).
    pub fn try_acquire(&self, id: ProcessInstanceId) -> Result<InstanceLease, InstanceLockedError> {
        let mut held = self
            .held
            .lock()
            .expect("мьютекс лизов не должен быть отравлен");
        if !held.insert(id.clone()) {
            return Err(InstanceLockedError(id));
        }
        Ok(InstanceLease {
            held: self.held.clone(),
            id,
        })
    }
}

impl Drop for InstanceLease {
    fn drop(&mut self) {
        if let Ok(mut held) = self.held.lock() {
            held.remove(&self.id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acquiring_an_unlocked_instance_succeeds() {
        let locks = InstanceWriteLocks::new();
        assert!(locks.try_acquire(ProcessInstanceId("a".into())).is_ok());
    }

    #[test]
    fn acquiring_an_already_locked_instance_fails() {
        let locks = InstanceWriteLocks::new();
        let _lease = locks.try_acquire(ProcessInstanceId("a".into())).unwrap();

        let result = locks.try_acquire(ProcessInstanceId("a".into()));

        assert_eq!(
            result.unwrap_err(),
            InstanceLockedError(ProcessInstanceId("a".into()))
        );
    }

    #[test]
    fn dropping_the_lease_releases_the_instance() {
        let locks = InstanceWriteLocks::new();
        {
            let _lease = locks.try_acquire(ProcessInstanceId("a".into())).unwrap();
        } // лиз освобождён здесь

        assert!(locks.try_acquire(ProcessInstanceId("a".into())).is_ok());
    }

    #[test]
    fn different_instances_can_be_locked_independently() {
        let locks = InstanceWriteLocks::new();
        let _lease_a = locks.try_acquire(ProcessInstanceId("a".into())).unwrap();

        assert!(locks.try_acquire(ProcessInstanceId("b".into())).is_ok());
    }

    #[test]
    fn cloned_registries_share_the_same_lock_state() {
        // Так реестр используется в реальности: несколько независимых
        // вызывающих держат клоны одного и того же реестра.
        let locks = InstanceWriteLocks::new();
        let locks_clone = locks.clone();
        let _lease = locks.try_acquire(ProcessInstanceId("a".into())).unwrap();

        let result = locks_clone.try_acquire(ProcessInstanceId("a".into()));

        assert!(result.is_err(), "клон реестра обязан видеть лиз оригинала");
    }
}
