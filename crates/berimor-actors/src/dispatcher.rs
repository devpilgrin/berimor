//! Диспетчер: доска задач, назначение по правилам, лимит очереди human_gate.
//!
//! Источник: `ideal-agent-architecture.md` §3.8. ROADMAP: A3 (назначение/эскалация).
//!
//! Лимит очереди `human_gate` + статус `throttled` (A4, ADR-0015) — вне
//! scope: заблокирована P7 (шаг `human_gate` с политикой таймаута
//! эскалации в Process Engine ещё не реализован как отдельная задача).

use std::collections::HashMap;

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
}

/// Доска задач + правила назначения + эскалация после N неудач подряд.
pub struct Dispatcher {
    tasks: HashMap<TaskId, Task>,
    max_attempts: u32,
}

impl Dispatcher {
    /// `max_attempts` — сколько неудач подряд допустимо, прежде чем
    /// задача эскалируется человеку (§3.8: «после заданного числа
    /// неудач»); `0` значит эскалация с первой же неудачи.
    pub fn new(max_attempts: u32) -> Self {
        Self {
            tasks: HashMap::new(),
            max_attempts,
        }
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
}
