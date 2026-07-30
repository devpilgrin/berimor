//! Планировщик: персистентный min-heap времён срабатывания, защита от
//! двойного тика, статус `throttled` при заблокированной очереди `human_gate`.
//!
//! Источник: `ideal-agent-architecture.md` §3.8, `stack.md` §69,
//! `process-engine.md` §5, ADR-0015. ROADMAP: A5 · A4 (вторая половина —
//! `throttled`, первая — `crate::dispatcher::Dispatcher::with_human_gate_limit`).
//!
//! Персистентность и защита от двойного тика — уже в
//! `berimor_storage::ScheduleStore` (одна SQL-транзакция атомарно
//! выбирает и продвигает сработавшие расписания). Этот модуль — то, что
//! стоит НАД хранилищем: валидация расписания при создании
//! («невозможное расписание отклоняется... а не тикает вечно», §3.8),
//! периодический тик через таймер tokio, и (A4) гейт `human_gate`:
//! «расписания, упирающиеся в неразобранные human_gate, получают статус
//! throttled, а не отменяются и не продолжают тикать вхолостую» (§5) —
//! реализовано через `ScheduleStore::due` (peek, не pop): при закрытом
//! гейте расписание остаётся due в хранилище и пробует снова на
//! следующем тике, вместо того чтобы быть продвинутым/удалённым вхолостую.

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

    /// Тик с учётом гейта `human_gate` (A4, ADR-0015). `is_throttled` —
    /// решение вызывающего кода на этот момент (обычно
    /// `dispatcher.open_escalations_count() >= limit`); планировщик сам не
    /// знает о диспетчере, только реагирует на булев сигнал. Когда
    /// закрыт — расписания, готовые сработать, НЕ продвигаются
    /// (`ScheduleStore::due`, не `tick`): они остаются due и будут снова
    /// предложены на следующем тике, когда гейт откроется — «не
    /// отменяются и не тикают вхолостую» (§5).
    pub fn tick_at_gated(
        &self,
        now_ms: i64,
        is_throttled: bool,
    ) -> Result<TickOutcome, StorageError> {
        if is_throttled {
            Ok(TickOutcome::Throttled(self.store.due(now_ms)?))
        } else {
            Ok(TickOutcome::Fired(self.store.tick(now_ms)?))
        }
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

    /// Вариант [`Scheduler::run`] с гейтом `human_gate` (A4): `is_throttled`
    /// вызывается заново на каждом тике — отражает актуальное состояние
    /// очереди диспетчера на этот момент, не снимок на момент запуска.
    /// `on_fire` — как в `run`; `on_throttled` вызывается на каждое
    /// расписание, которое было due, но не продвинуто из-за закрытого
    /// гейта.
    pub async fn run_gated(
        &self,
        period: Duration,
        is_throttled: impl Fn() -> bool + Send + Sync,
        on_fire: impl Fn(Schedule) + Send + Sync,
        on_throttled: impl Fn(&Schedule) + Send + Sync,
    ) {
        let mut interval = tokio::time::interval(period);
        loop {
            interval.tick().await;
            match self.tick_at_gated(now_ms(), is_throttled()) {
                Ok(TickOutcome::Fired(fired)) => {
                    for schedule in fired {
                        on_fire(schedule);
                    }
                }
                Ok(TickOutcome::Throttled(due)) => {
                    for schedule in &due {
                        on_throttled(schedule);
                    }
                }
                Err(_) => {}
            }
        }
    }
}

/// Результат [`Scheduler::tick_at_gated`] — сработало реально
/// (продвинуто/удалено в хранилище) или было бы due, но заблокировано
/// закрытым гейтом `human_gate` (ничего не изменилось в хранилище).
#[derive(Debug, Clone, PartialEq)]
pub enum TickOutcome {
    Fired(Vec<Schedule>),
    Throttled(Vec<Schedule>),
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

    #[test]
    fn tick_at_gated_not_throttled_behaves_like_tick_at() {
        let store = Arc::new(SqliteEventLog::open_in_memory().unwrap());
        let scheduler = Scheduler::new(store);
        scheduler.submit(one_shot("s-1", 1000)).unwrap();

        let outcome = scheduler.tick_at_gated(1000, false).unwrap();

        assert_eq!(outcome, TickOutcome::Fired(vec![one_shot("s-1", 1000)]));
    }

    #[test]
    fn tick_at_gated_throttled_does_not_consume_the_schedule() {
        let store = Arc::new(SqliteEventLog::open_in_memory().unwrap());
        let scheduler = Scheduler::new(store);
        scheduler.submit(one_shot("s-1", 1000)).unwrap();

        let outcome = scheduler.tick_at_gated(1000, true).unwrap();
        assert_eq!(outcome, TickOutcome::Throttled(vec![one_shot("s-1", 1000)]));

        // Не потреблено гейтом — обычный тик всё ещё находит и продвигает его.
        let fired = scheduler.tick_at(1000).unwrap();
        assert_eq!(fired.len(), 1);
    }

    #[test]
    fn tick_at_gated_throttled_with_nothing_due_returns_empty_throttled() {
        let store = Arc::new(SqliteEventLog::open_in_memory().unwrap());
        let scheduler = Scheduler::new(store);
        scheduler.submit(one_shot("s-1", 5000)).unwrap();

        let outcome = scheduler.tick_at_gated(1000, true).unwrap();

        assert_eq!(outcome, TickOutcome::Throttled(vec![]));
    }

    #[tokio::test(start_paused = true)]
    async fn run_gated_throttled_calls_on_throttled_and_never_on_fire() {
        let store = Arc::new(SqliteEventLog::open_in_memory().unwrap());
        let scheduler = Scheduler::new(store);
        scheduler.submit(one_shot("s-1", 1)).unwrap();

        let fired: Arc<Mutex<Vec<Schedule>>> = Arc::new(Mutex::new(Vec::new()));
        let throttled: Arc<Mutex<Vec<Schedule>>> = Arc::new(Mutex::new(Vec::new()));
        let fired_clone = fired.clone();
        let throttled_clone = throttled.clone();
        let run = tokio::spawn(async move {
            scheduler
                .run_gated(
                    Duration::from_millis(10),
                    || true,
                    move |schedule| fired_clone.lock().unwrap().push(schedule),
                    move |schedule| throttled_clone.lock().unwrap().push(schedule.clone()),
                )
                .await;
        });

        tokio::time::advance(Duration::from_millis(15)).await;
        tokio::task::yield_now().await;
        run.abort();

        assert!(fired.lock().unwrap().is_empty());
        assert_eq!(throttled.lock().unwrap().len(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn run_gated_not_throttled_fires_and_never_calls_on_throttled() {
        let store = Arc::new(SqliteEventLog::open_in_memory().unwrap());
        let scheduler = Scheduler::new(store);
        scheduler.submit(one_shot("s-1", 1)).unwrap();

        let fired: Arc<Mutex<Vec<Schedule>>> = Arc::new(Mutex::new(Vec::new()));
        let throttled: Arc<Mutex<Vec<Schedule>>> = Arc::new(Mutex::new(Vec::new()));
        let fired_clone = fired.clone();
        let throttled_clone = throttled.clone();
        let run = tokio::spawn(async move {
            scheduler
                .run_gated(
                    Duration::from_millis(10),
                    || false,
                    move |schedule| fired_clone.lock().unwrap().push(schedule),
                    move |schedule| throttled_clone.lock().unwrap().push(schedule.clone()),
                )
                .await;
        });

        tokio::time::advance(Duration::from_millis(15)).await;
        tokio::task::yield_now().await;
        run.abort();

        assert_eq!(fired.lock().unwrap().len(), 1);
        assert!(throttled.lock().unwrap().is_empty());
    }
}
