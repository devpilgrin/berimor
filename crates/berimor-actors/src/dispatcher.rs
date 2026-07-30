//! Диспетчер: доска задач, назначение по правилам, лимит очереди human_gate.
//!
//! Источник: `ideal-agent-architecture.md` §3.8, `process-engine.md` §5,
//! ADR-0015. ROADMAP: A3 (назначение/эскалация) · A4 (лимит очереди
//! `human_gate`).
//!
//! «После заданного числа неудач задача блокируется и уходит человеку»
//! (§3.8) — `TaskStatus::Escalated` уже и есть та самая остановка
//! `human_gate`: отдельного типа для неё не заводим (ADR-0015 говорит о
//! пределе на диспетчере, не о новом виде состояния). Статус `throttled`
//! для расписаний — в `crate::scheduler`, диспетчер лишь считает открытые
//! эскалации и решает, пускать ли новые назначения.

use std::collections::{HashMap, HashSet};

/// Идентификатор актора — та же строка, что и `actor::ActorId`; отдельный
/// алиас, не тип из `actor.rs`, чтобы диспетчер не зависел от tokio
/// (актор — задача tokio, диспетчер — чистая доска, синхронная логика).
pub type ActorId = String;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TaskId(pub String);

#[derive(Debug, Clone, PartialEq)]
pub enum TaskStatus {
    Pending,
    Assigned {
        actor: ActorId,
    },
    /// Назначенному актору не удалось выполнить задачу; `attempts` —
    /// сколько раз подряд.
    Failed {
        attempts: u32,
    },
    /// Эскалация после `attempts` неудач — задача уходит человеку, не
    /// назначается акторам снова автоматически (§3.8: «после заданного
    /// числа неудач задача блокируется и уходит человеку»).
    Escalated {
        attempts: u32,
    },
    Done,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Task {
    pub id: TaskId,
    /// Класс задачи, по которому применяются правила назначения.
    pub topic: String,
    pub status: TaskStatus,
}

/// Правило назначения — код-правило (§3.8: «назначение акторам по
/// декларативным правилам в коде»), не решение модели.
pub trait AssignmentRule {
    fn assign(&self, task: &Task, available: &[ActorId]) -> Option<ActorId>;
}

/// Простейшее правило: первый доступный актор, без учёта `topic` — для
/// вызывающего кода, которому пока не нужна маршрутизация по классу
/// задачи.
pub struct FirstAvailable;

impl AssignmentRule for FirstAvailable {
    fn assign(&self, _task: &Task, available: &[ActorId]) -> Option<ActorId> {
        available.first().cloned()
    }
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum DispatcherError {
    #[error("задача {0:?} не найдена на доске")]
    UnknownTask(TaskId),
    #[error("задача {0:?} уже существует на доске")]
    DuplicateTask(TaskId),
    /// ADR-0015: предел одновременных эскалаций `human_gate` достигнут —
    /// диспетчер не назначает новые задачи, пока человек не разберёт
    /// очередь (`Dispatcher::resolve_escalation`). Код-правило, не решение
    /// модели («диспетчер прекращает назначать акторам новые задачи,
    /// ведущие к эскалации, до разбора очереди человеком»).
    #[error(
        "очередь подтверждений human_gate заполнена (предел {limit}) — новые задачи не назначаются"
    )]
    HumanGateQueueFull { limit: u32 },
    /// `resolve_escalation` вызван для задачи, которая не в состоянии
    /// `Escalated` — разбирать нечего.
    #[error("задача {0:?} не находится в состоянии эскалации")]
    NotEscalated(TaskId),
}

/// Доска задач + правила назначения + эскалация после N неудач подряд +
/// предел очереди `human_gate` (A4, опционально — см. [`Dispatcher::new`]).
pub struct Dispatcher {
    tasks: HashMap<TaskId, Task>,
    max_attempts: u32,
    /// `None` — предел не включён: вызывающий код явно не запросил защиту
    /// ADR-0015 (например, тестовый/однопользовательский сценарий без
    /// реальной очереди подтверждений). `Some(n)` — не больше `n`
    /// одновременных эскалаций на этом диспетчере.
    human_gate_limit: Option<u32>,
    open_escalations: HashSet<TaskId>,
}

impl Dispatcher {
    /// `max_attempts` — сколько неудач подряд допустимо, прежде чем
    /// задача эскалируется человеку (§3.8: «после заданного числа
    /// неудач»); `0` значит эскалация с первой же неудачи. Предел очереди
    /// `human_gate` не включён — см. [`Dispatcher::with_human_gate_limit`].
    pub fn new(max_attempts: u32) -> Self {
        Self {
            tasks: HashMap::new(),
            max_attempts,
            human_gate_limit: None,
            open_escalations: HashSet::new(),
        }
    }

    /// Диспетчер с включённым пределом очереди `human_gate` (ADR-0015,
    /// A4): не больше `human_gate_limit` одновременных эскалаций на
    /// арендатора/очередь — при достижении предела [`Dispatcher::assign`]
    /// отказывает в назначении любой ещё не решённой задачи (заранее
    /// неизвестно, какая из pending-задач в итоге провалится и
    /// эскалирует, поэтому останавливаются все, а не только «подозрительные»),
    /// пока человек не разберёт хотя бы одну через
    /// [`Dispatcher::resolve_escalation`].
    pub fn with_human_gate_limit(max_attempts: u32, human_gate_limit: u32) -> Self {
        Self {
            tasks: HashMap::new(),
            max_attempts,
            human_gate_limit: Some(human_gate_limit),
            open_escalations: HashSet::new(),
        }
    }

    /// Число задач, сейчас ожидающих разбора человеком (`Escalated`, ещё
    /// не закрытых через `resolve_escalation`) — видимость очереди для
    /// вызывающего кода (UI/CLI), не только внутренний счётчик гейта.
    pub fn open_escalations_count(&self) -> usize {
        self.open_escalations.len()
    }

    pub fn submit(&mut self, id: TaskId, topic: String) -> Result<(), DispatcherError> {
        if self.tasks.contains_key(&id) {
            return Err(DispatcherError::DuplicateTask(id));
        }
        self.tasks.insert(
            id.clone(),
            Task {
                id,
                topic,
                status: TaskStatus::Pending,
            },
        );
        Ok(())
    }

    pub fn task(&self, id: &TaskId) -> Option<&Task> {
        self.tasks.get(id)
    }

    /// Назначает задачу актором по `rule`. Эскалированные и завершённые
    /// задачи не переназначаются — `Ok(None)` без обращения к `rule`.
    /// Если предел очереди `human_gate` включён и достигнут — `Err`
    /// раньше `rule`, ещё не решённые задачи не трогает (A4).
    pub fn assign(
        &mut self,
        id: &TaskId,
        rule: &dyn AssignmentRule,
        available: &[ActorId],
    ) -> Result<Option<ActorId>, DispatcherError> {
        let task = self
            .tasks
            .get(id)
            .ok_or_else(|| DispatcherError::UnknownTask(id.clone()))?;
        if matches!(task.status, TaskStatus::Escalated { .. } | TaskStatus::Done) {
            return Ok(None);
        }
        if let Some(limit) = self.human_gate_limit {
            if self.open_escalations.len() as u32 >= limit {
                return Err(DispatcherError::HumanGateQueueFull { limit });
            }
        }
        let assigned = rule.assign(task, available);
        if let Some(actor) = &assigned {
            self.tasks.get_mut(id).unwrap().status = TaskStatus::Assigned {
                actor: actor.clone(),
            };
        }
        Ok(assigned)
    }

    /// Регистрирует неудачу: увеличивает счётчик попыток подряд,
    /// эскалирует при достижении `max_attempts`.
    pub fn record_failure(&mut self, id: &TaskId) -> Result<&TaskStatus, DispatcherError> {
        let task = self
            .tasks
            .get_mut(id)
            .ok_or_else(|| DispatcherError::UnknownTask(id.clone()))?;
        let attempts = match &task.status {
            TaskStatus::Failed { attempts } => attempts + 1,
            _ => 1,
        };
        task.status = if attempts >= self.max_attempts {
            TaskStatus::Escalated { attempts }
        } else {
            TaskStatus::Failed { attempts }
        };
        if matches!(task.status, TaskStatus::Escalated { .. }) {
            self.open_escalations.insert(id.clone());
        }
        Ok(&self.tasks[id].status)
    }

    pub fn record_success(&mut self, id: &TaskId) -> Result<(), DispatcherError> {
        let task = self
            .tasks
            .get_mut(id)
            .ok_or_else(|| DispatcherError::UnknownTask(id.clone()))?;
        task.status = TaskStatus::Done;
        Ok(())
    }

    /// Человек разобрал эскалацию — освобождает место в очереди (ADR-0015)
    /// и переводит задачу в `Done`. Если разбор означает «сделать
    /// заново», вызывающий код подаёт новую задачу — эта не переоткрывается
    /// автоматически (то же решение, что `record_success`).
    pub fn resolve_escalation(&mut self, id: &TaskId) -> Result<(), DispatcherError> {
        let task = self
            .tasks
            .get_mut(id)
            .ok_or_else(|| DispatcherError::UnknownTask(id.clone()))?;
        if !matches!(task.status, TaskStatus::Escalated { .. }) {
            return Err(DispatcherError::NotEscalated(id.clone()));
        }
        task.status = TaskStatus::Done;
        self.open_escalations.remove(id);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn submit_then_task_round_trips_as_pending() {
        let mut d = Dispatcher::new(3);
        d.submit(TaskId("t-1".into()), "topic-a".into()).unwrap();

        assert_eq!(
            d.task(&TaskId("t-1".into())).unwrap().status,
            TaskStatus::Pending
        );
    }

    #[test]
    fn submit_duplicate_id_is_an_error() {
        let mut d = Dispatcher::new(3);
        d.submit(TaskId("t-1".into()), "topic-a".into()).unwrap();

        assert_eq!(
            d.submit(TaskId("t-1".into()), "topic-a".into()),
            Err(DispatcherError::DuplicateTask(TaskId("t-1".into())))
        );
    }

    #[test]
    fn assign_with_available_actor_transitions_to_assigned() {
        let mut d = Dispatcher::new(3);
        d.submit(TaskId("t-1".into()), "topic-a".into()).unwrap();

        let actor = d
            .assign(&TaskId("t-1".into()), &FirstAvailable, &["actor-a".into()])
            .unwrap();

        assert_eq!(actor, Some("actor-a".into()));
        assert_eq!(
            d.task(&TaskId("t-1".into())).unwrap().status,
            TaskStatus::Assigned {
                actor: "actor-a".into()
            }
        );
    }

    #[test]
    fn assign_with_no_available_actor_leaves_task_unassigned() {
        let mut d = Dispatcher::new(3);
        d.submit(TaskId("t-1".into()), "topic-a".into()).unwrap();

        let actor = d
            .assign(&TaskId("t-1".into()), &FirstAvailable, &[])
            .unwrap();

        assert_eq!(actor, None);
        assert_eq!(
            d.task(&TaskId("t-1".into())).unwrap().status,
            TaskStatus::Pending
        );
    }

    #[test]
    fn assign_unknown_task_is_an_error() {
        let mut d = Dispatcher::new(3);
        assert_eq!(
            d.assign(&TaskId("no-such-task".into()), &FirstAvailable, &[]),
            Err(DispatcherError::UnknownTask(TaskId("no-such-task".into())))
        );
    }

    #[test]
    fn record_failure_increments_attempts_below_threshold() {
        let mut d = Dispatcher::new(3);
        d.submit(TaskId("t-1".into()), "topic-a".into()).unwrap();
        d.assign(&TaskId("t-1".into()), &FirstAvailable, &["actor-a".into()])
            .unwrap();

        let status = d.record_failure(&TaskId("t-1".into())).unwrap();
        assert_eq!(*status, TaskStatus::Failed { attempts: 1 });
    }

    #[test]
    fn record_failure_reaching_max_attempts_escalates() {
        let mut d = Dispatcher::new(2);
        d.submit(TaskId("t-1".into()), "topic-a".into()).unwrap();
        d.assign(&TaskId("t-1".into()), &FirstAvailable, &["actor-a".into()])
            .unwrap();

        d.record_failure(&TaskId("t-1".into())).unwrap();
        let status = d.record_failure(&TaskId("t-1".into())).unwrap();

        assert_eq!(*status, TaskStatus::Escalated { attempts: 2 });
    }

    #[test]
    fn max_attempts_zero_escalates_on_the_first_failure() {
        let mut d = Dispatcher::new(0);
        d.submit(TaskId("t-1".into()), "topic-a".into()).unwrap();
        d.assign(&TaskId("t-1".into()), &FirstAvailable, &["actor-a".into()])
            .unwrap();

        let status = d.record_failure(&TaskId("t-1".into())).unwrap();
        assert_eq!(*status, TaskStatus::Escalated { attempts: 1 });
    }

    #[test]
    fn escalated_task_is_not_reassigned() {
        let mut d = Dispatcher::new(1);
        d.submit(TaskId("t-1".into()), "topic-a".into()).unwrap();
        d.assign(&TaskId("t-1".into()), &FirstAvailable, &["actor-a".into()])
            .unwrap();
        d.record_failure(&TaskId("t-1".into())).unwrap();

        let actor = d
            .assign(&TaskId("t-1".into()), &FirstAvailable, &["actor-a".into()])
            .unwrap();

        assert_eq!(
            actor, None,
            "эскалированная задача не переназначается автоматически"
        );
    }

    #[test]
    fn record_success_transitions_to_done() {
        let mut d = Dispatcher::new(3);
        d.submit(TaskId("t-1".into()), "topic-a".into()).unwrap();
        d.assign(&TaskId("t-1".into()), &FirstAvailable, &["actor-a".into()])
            .unwrap();

        d.record_success(&TaskId("t-1".into())).unwrap();

        assert_eq!(
            d.task(&TaskId("t-1".into())).unwrap().status,
            TaskStatus::Done
        );
    }

    #[test]
    fn done_task_is_not_reassigned() {
        let mut d = Dispatcher::new(3);
        d.submit(TaskId("t-1".into()), "topic-a".into()).unwrap();
        d.assign(&TaskId("t-1".into()), &FirstAvailable, &["actor-a".into()])
            .unwrap();
        d.record_success(&TaskId("t-1".into())).unwrap();

        let actor = d
            .assign(&TaskId("t-1".into()), &FirstAvailable, &["actor-b".into()])
            .unwrap();

        assert_eq!(actor, None);
    }

    #[test]
    fn dispatcher_without_human_gate_limit_never_blocks_new_assignments() {
        // `new` (без лимита) — старое поведение A3 сохраняется буквально.
        let mut d = Dispatcher::new(1);
        d.submit(TaskId("t-1".into()), "topic-a".into()).unwrap();
        d.assign(&TaskId("t-1".into()), &FirstAvailable, &["actor-a".into()])
            .unwrap();
        d.record_failure(&TaskId("t-1".into())).unwrap();

        d.submit(TaskId("t-2".into()), "topic-a".into()).unwrap();
        let assigned = d
            .assign(&TaskId("t-2".into()), &FirstAvailable, &["actor-a".into()])
            .unwrap();

        assert_eq!(assigned, Some("actor-a".into()));
    }

    #[test]
    fn human_gate_limit_blocks_new_assignment_once_queue_is_full() {
        let mut d = Dispatcher::with_human_gate_limit(1, 1);
        d.submit(TaskId("t-1".into()), "topic-a".into()).unwrap();
        d.assign(&TaskId("t-1".into()), &FirstAvailable, &["actor-a".into()])
            .unwrap();
        d.record_failure(&TaskId("t-1".into())).unwrap(); // эскалирует, открытых = 1 == предел

        d.submit(TaskId("t-2".into()), "topic-a".into()).unwrap();
        let result = d.assign(&TaskId("t-2".into()), &FirstAvailable, &["actor-a".into()]);

        assert_eq!(
            result,
            Err(DispatcherError::HumanGateQueueFull { limit: 1 })
        );
    }

    #[test]
    fn resolve_escalation_frees_queue_capacity_for_new_assignments() {
        let mut d = Dispatcher::with_human_gate_limit(1, 1);
        d.submit(TaskId("t-1".into()), "topic-a".into()).unwrap();
        d.assign(&TaskId("t-1".into()), &FirstAvailable, &["actor-a".into()])
            .unwrap();
        d.record_failure(&TaskId("t-1".into())).unwrap();
        d.submit(TaskId("t-2".into()), "topic-a".into()).unwrap();
        assert!(d
            .assign(&TaskId("t-2".into()), &FirstAvailable, &["actor-a".into()])
            .is_err());

        d.resolve_escalation(&TaskId("t-1".into())).unwrap();

        let assigned = d
            .assign(&TaskId("t-2".into()), &FirstAvailable, &["actor-a".into()])
            .unwrap();
        assert_eq!(assigned, Some("actor-a".into()));
    }

    #[test]
    fn resolve_escalation_transitions_task_to_done() {
        let mut d = Dispatcher::with_human_gate_limit(1, 5);
        d.submit(TaskId("t-1".into()), "topic-a".into()).unwrap();
        d.assign(&TaskId("t-1".into()), &FirstAvailable, &["actor-a".into()])
            .unwrap();
        d.record_failure(&TaskId("t-1".into())).unwrap();

        d.resolve_escalation(&TaskId("t-1".into())).unwrap();

        assert_eq!(
            d.task(&TaskId("t-1".into())).unwrap().status,
            TaskStatus::Done
        );
    }

    #[test]
    fn resolve_escalation_on_non_escalated_task_is_an_error() {
        let mut d = Dispatcher::with_human_gate_limit(3, 5);
        d.submit(TaskId("t-1".into()), "topic-a".into()).unwrap();

        let result = d.resolve_escalation(&TaskId("t-1".into()));

        assert_eq!(
            result,
            Err(DispatcherError::NotEscalated(TaskId("t-1".into())))
        );
    }

    #[test]
    fn resolve_escalation_on_unknown_task_is_an_error() {
        let mut d = Dispatcher::with_human_gate_limit(3, 5);

        let result = d.resolve_escalation(&TaskId("no-such-task".into()));

        assert_eq!(
            result,
            Err(DispatcherError::UnknownTask(TaskId("no-such-task".into())))
        );
    }

    #[test]
    fn open_escalations_count_tracks_outstanding_escalations() {
        let mut d = Dispatcher::with_human_gate_limit(1, 5);
        d.submit(TaskId("t-1".into()), "topic-a".into()).unwrap();
        d.assign(&TaskId("t-1".into()), &FirstAvailable, &["actor-a".into()])
            .unwrap();
        assert_eq!(d.open_escalations_count(), 0);

        d.record_failure(&TaskId("t-1".into())).unwrap();

        assert_eq!(d.open_escalations_count(), 1);
    }

    #[test]
    fn human_gate_limit_does_not_block_reassigning_escalated_or_done_tasks() {
        // Запрос на уже эскалированную/завершённую задачу — это не "новое
        // назначение, ведущее к эскалации", это опрос уже решённого пути;
        // гейт не должен мешать даже когда очередь полна.
        let mut d = Dispatcher::with_human_gate_limit(1, 1);
        d.submit(TaskId("t-1".into()), "topic-a".into()).unwrap();
        d.assign(&TaskId("t-1".into()), &FirstAvailable, &["actor-a".into()])
            .unwrap();
        d.record_failure(&TaskId("t-1".into())).unwrap(); // очередь полна (1/1)

        let result = d.assign(&TaskId("t-1".into()), &FirstAvailable, &["actor-a".into()]);

        assert_eq!(result, Ok(None));
    }
}
