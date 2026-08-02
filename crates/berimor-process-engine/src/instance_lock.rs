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

/// Межпроцессный лиз (аудит 1.11): адвизорная `flock`-блокировка файла на
/// инстанс — два ОТДЕЛЬНЫХ процесса CLI, восстановивших один инстанс,
/// больше не продвигают его параллельно. Снимается ядром при закрытии
/// дескриптора (смерть процесса включительно) — залоченный навсегда
/// инстанс после падения невозможен, уборки протухших файлов не нужно.
pub struct FileInstanceLease {
    _file: std::fs::File,
}

#[derive(Debug, thiserror::Error)]
pub enum FileLeaseError {
    #[error("инстанс {0:?} уже продвигается другим процессом")]
    Locked(ProcessInstanceId),
    #[error("лок-файл инстанса: {0}")]
    Io(#[from] std::io::Error),
}

/// Берёт межпроцессный лиз: `<locks_dir>/<sanitized-id>.lock` +
/// `try_lock_exclusive` — отказ второму процессу немедленно (та же
/// семантика «не очередь, а отказ», что у in-memory лиза выше).
pub fn try_acquire_file_lease(
    locks_dir: &std::path::Path,
    id: &ProcessInstanceId,
) -> Result<FileInstanceLease, FileLeaseError> {
    use fs2::FileExt;
    std::fs::create_dir_all(locks_dir)?;
    let path = locks_dir.join(format!("{}.lock", sanitize_id(id)));
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(path)?;
    file.try_lock_exclusive()
        .map_err(|_| FileLeaseError::Locked(id.clone()))?;
    Ok(FileInstanceLease { _file: file })
}

/// Id инстанса — произвольная строка; для имени файла оставляем только
/// безопасный алфавит.
fn sanitize_id(id: &ProcessInstanceId) -> String {
    id.0.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

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

    /// Аудит 1.11: файловый лиз конфликтует даже внутри одного процесса
    /// (flock — на открытое описание файла; два open() — две блокировки)
    /// — тем более между двумя процессами CLI.
    #[test]
    fn file_lease_refuses_second_holder() {
        let dir = std::env::temp_dir().join(format!("berimor-lease-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let id = ProcessInstanceId("inst-1".into());

        let first = try_acquire_file_lease(&dir, &id).unwrap();
        let second = try_acquire_file_lease(&dir, &id);
        assert!(matches!(second, Err(FileLeaseError::Locked(_))));

        drop(first);
        let third = try_acquire_file_lease(&dir, &id);
        assert!(third.is_ok(), "после Drop лиз обязан освобождаться");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Другой инстанс — другой лок-файл, конфликта нет.
    #[test]
    fn file_leases_of_different_instances_do_not_conflict() {
        let dir = std::env::temp_dir().join(format!("berimor-lease2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let _a = try_acquire_file_lease(&dir, &ProcessInstanceId("a".into())).unwrap();
        let b = try_acquire_file_lease(&dir, &ProcessInstanceId("b".into()));
        assert!(b.is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
