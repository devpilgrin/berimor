//! Планировщик: персистентный min-heap времён срабатывания, защита от двойного тика.
//!
//! Источник: `ideal-agent-architecture.md` §3.8, `stack.md` §69. ROADMAP: A5.
//!
//! Персистентность и защита от двойного тика — уже в
//! `berimor_storage::ScheduleStore` (одна SQL-транзакция атомарно
//! выбирает и продвигает сработавшие расписания). Этот модуль — то, что
//! стоит НАД хранилищем: валидация расписания при создании
//! («невозможное расписание отклоняется... а не тикает вечно», §3.8) и
//! периодический тик через таймер tokio.

use berimor_storage::{Schedule, ScheduleId, ScheduleStore, StorageError};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Debug, thiserror::Error)]
pub enum ScheduleValidationError {
    #[error("интервал повторяющегося расписания обязан быть положительным, получено {0} мс")]
    NonPositiveInterval(i64),
}

/// Проверяет расписание ДО записи в хранилище — «невозможное расписание
/// отклоняется при создании» (§3.8): неположительный интервал повторения
/// означал бы срабатывание на каждом же тике бесконечно — то самое
/// «тикает вечно», которого документ требует избегать.
pub fn validate(schedule: &Schedule) -> Result<(), ScheduleValidationError> {
    if let Some(interval) = schedule.interval_ms {
        if interval <= 0 {
            return Err(ScheduleValidationError::NonPositiveInterval(interval));
        }
    }
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum SubmitError {
    #[error(transparent)]
    Invalid(#[from] ScheduleValidationError),
    #[error(transparent)]
    Storage(#[from] StorageError),
}

/// Обёртка над `ScheduleStore`: валидация на запись + периодический тик.
pub struct Scheduler<S: ScheduleStore + Send + Sync> {
    store: Arc<S>,
}

impl<S: ScheduleStore + Send + Sync> Scheduler<S> {
    pub fn new(store: Arc<S>) -> Self {
        Self { store }
    }

    pub fn submit(&self, schedule: Schedule) -> Result<(), SubmitError> {
        validate(&schedule)?;
        self.store.upsert_schedule(&schedule)?;
        Ok(())
    }

    pub fn cancel(&self, id: &ScheduleId) -> Result<(), StorageError> {
        self.store.cancel_schedule(id)
    }

    /// Один тик на заданный момент — возвращает сработавшие расписания.
    /// Вызывающий код решает, что делать со сработавшим payload, не этот
    /// метод (то же разделение, что `StepExecutor`/`EnvelopeHandler`:
    /// общая механика здесь, конкретное поведение — снаружи).
    pub fn tick_at(&self, now_ms: i64) -> Result<Vec<Schedule>, StorageError> {
        self.store.tick(now_ms)
    }

    /// Бесконечный цикл тиков на интервале `period` — задача tokio (тот
    /// же приём, что `Actor::run`, A1). `on_fire` вызывается на каждое
    /// сработавшее расписание каждого тика; ошибка хранилища на
    /// отдельном тике не останавливает цикл — расписание остаётся due,
    /// следующий тик попробует снова, ничего не теряется.
    pub async fn run(&self, period: Duration, on_fire: impl Fn(Schedule) + Send + Sync) {
        let mut interval = tokio::time::interval(period);
        loop {
            interval.tick().await;
            if let Ok(fired) = self.store.tick(now_ms()) {
                for schedule in fired {
                    on_fire(schedule);
                }
            }
        }
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use berimor_storage::SqliteEventLog;
    use serde_json::json;
    use std::sync::Mutex;

    fn one_shot(id: &str, next_fire_ms: i64) -> Schedule {
        Schedule {
            id: ScheduleId(id.into()),
            next_fire_ms,
            interval_ms: None,
            payload: json!({"kind": "one-shot"}),
        }
    }

    fn recurring(id: &str, next_fire_ms: i64, interval_ms: i64) -> Schedule {
        Schedule {
            id: ScheduleId(id.into()),
            next_fire_ms,
            interval_ms: Some(interval_ms),
            payload: json!({"kind": "recurring"}),
        }
    }

    #[test]
    fn validate_accepts_one_shot_schedule() {
        assert!(validate(&one_shot("s-1", 1000)).is_ok());
    }

    #[test]
    fn validate_accepts_positive_interval() {
        assert!(validate(&recurring("s-1", 1000, 500)).is_ok());
    }

    #[test]
    fn validate_rejects_zero_interval() {
        assert!(matches!(
            validate(&recurring("s-1", 1000, 0)),
            Err(ScheduleValidationError::NonPositiveInterval(0))
        ));
    }

    #[test]
    fn validate_rejects_negative_interval() {
        assert!(matches!(
            validate(&recurring("s-1", 1000, -500)),
            Err(ScheduleValidationError::NonPositiveInterval(-500))
        ));
    }

    #[test]
    fn submit_rejects_invalid_schedule_before_touching_storage() {
        let store = Arc::new(SqliteEventLog::open_in_memory().unwrap());
        let scheduler = Scheduler::new(store.clone());

        let result = scheduler.submit(recurring("s-1", 1000, 0));

        assert!(matches!(result, Err(SubmitError::Invalid(_))));
        assert!(scheduler.tick_at(1000).unwrap().is_empty());
    }

    #[test]
    fn submit_then_tick_at_round_trips() {
        let store = Arc::new(SqliteEventLog::open_in_memory().unwrap());
        let scheduler = Scheduler::new(store);

        scheduler.submit(one_shot("s-1", 1000)).unwrap();
        let fired = scheduler.tick_at(1000).unwrap();

        assert_eq!(fired.len(), 1);
        assert_eq!(fired[0].id, ScheduleId("s-1".into()));
    }

    #[test]
    fn cancel_removes_schedule_before_it_fires() {
        let store = Arc::new(SqliteEventLog::open_in_memory().unwrap());
        let scheduler = Scheduler::new(store);

        scheduler.submit(one_shot("s-1", 1000)).unwrap();
        scheduler.cancel(&ScheduleId("s-1".into())).unwrap();

        assert!(scheduler.tick_at(1000).unwrap().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn run_fires_due_schedule_on_a_timer_tick() {
        let store = Arc::new(SqliteEventLog::open_in_memory().unwrap());
        let scheduler = Scheduler::new(store);
        // Срабатывает "всегда" (эпоха 0..сейчас всегда в прошлом) — тест
        // проверяет, что таймер вообще запускает tick(), не конкретный момент.
        scheduler.submit(one_shot("s-1", 1)).unwrap();

        let fired: Arc<Mutex<Vec<Schedule>>> = Arc::new(Mutex::new(Vec::new()));
        let fired_clone = fired.clone();
        let run = tokio::spawn(async move {
            scheduler
                .run(Duration::from_millis(10), move |schedule| {
                    fired_clone.lock().unwrap().push(schedule);
                })
                .await;
        });

        tokio::time::advance(Duration::from_millis(15)).await;
        tokio::task::yield_now().await;
        run.abort();

        assert_eq!(fired.lock().unwrap().len(), 1);
    }
}
