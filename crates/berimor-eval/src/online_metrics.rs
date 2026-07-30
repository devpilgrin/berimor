//! Пайплайн онлайн-метрик: доля успеха задач, скорость подтверждений (O4).
//!
//! Источник: `ideal-agent-architecture.md` §3.11 («медленный цикл —
//! онлайн-метрики: доля успешных задач, скорость подтверждений, отказы
//! медиации»). ROADMAP: O4.
//!
//! Чистая функция от уже собранных журналов инстансов — тот же приём,
//! что у O2/O3: сбор данных (какие инстансы попадают в окно метрик,
//! итерация по хранилищу) — забота вызывающего кода, не этого модуля.
//! `EventLog` (F1) не даёт способа перечислить все инстансы, только
//! `replay` одного — расширение хранилища ради этого вне scope задачи.
//!
//! «Отказы медиации» из цитаты §3.11 уже посчитаны M7
//! (`berimor_mediation::telemetry::RejectionStats`) — здесь то, чего у
//! M7 нет: доля успеха задач и скорость подтверждений `human_gate`.
//! Успех задачи ("дошёл до конца") нельзя восстановить из одного только
//! журнала: движок не журналирует отдельное событие завершения —
//! `RunOutcome::Finished` существует только как возврат `engine::run`,
//! поэтому исход инстанса — часть входных данных, не то, что этот
//! модуль умеет вывести сам.

use berimor_types::event::{Event, EventKind};

/// Итог одного инстанса, как его увидел вызывающий код (обычно —
/// `RunOutcome` из `berimor-process-engine`, приведённый к этим трём
/// случаям без зависимости от типа движка).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstanceOutcome {
    Finished,
    AwaitingHuman,
    /// Ни финиш, ни ожидание человека — инстанс оборван (сбой, лимит)
    /// или ещё выполняется в момент сбора метрик.
    Unresolved,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OnlineMetrics {
    pub task_success_rate: f64,
    /// Медиана задержек подтверждения (мс) по всем завершённым парам
    /// `HumanGateOpened`→`HumanGateResolved` во всех инстансах.
    /// `None` — таких пар не нашлось (открытий без ответа не считается).
    pub median_confirmation_latency_ms: Option<i64>,
}

/// Вычисляет метрики по набору `(журнал инстанса, исход инстанса)`.
pub fn compute(instances: &[(Vec<Event>, InstanceOutcome)]) -> OnlineMetrics {
    OnlineMetrics {
        task_success_rate: success_rate(instances),
        median_confirmation_latency_ms: median_confirmation_latency(instances),
    }
}

fn success_rate(instances: &[(Vec<Event>, InstanceOutcome)]) -> f64 {
    if instances.is_empty() {
        return 0.0;
    }
    let finished = instances
        .iter()
        .filter(|(_, outcome)| *outcome == InstanceOutcome::Finished)
        .count();
    finished as f64 / instances.len() as f64
}

fn median_confirmation_latency(instances: &[(Vec<Event>, InstanceOutcome)]) -> Option<i64> {
    let mut latencies: Vec<i64> = instances
        .iter()
        .flat_map(|(events, _)| confirmation_latencies(events))
        .collect();
    if latencies.is_empty() {
        return None;
    }
    latencies.sort_unstable();
    Some(latencies[latencies.len() / 2])
}

/// Времена между каждым `HumanGateOpened` и следующим за ним по порядку
/// журнала `HumanGateResolved` — открытие без ответа (человек ещё не
/// решил) не попадает в выборку: не `0`, не бесконечность, просто не
/// готовое наблюдение.
fn confirmation_latencies(events: &[Event]) -> Vec<i64> {
    let mut latencies = Vec::new();
    let mut opened_at: Option<i64> = None;
    for event in events {
        match &event.kind {
            EventKind::HumanGateOpened { .. } => opened_at = Some(event.ts_ms),
            EventKind::HumanGateResolved => {
                if let Some(opened_ts) = opened_at.take() {
                    latencies.push(event.ts_ms - opened_ts);
                }
            }
            _ => {}
        }
    }
    latencies
}

#[cfg(test)]
mod tests {
    use super::*;
    use berimor_types::event::{EventSeq, ProcessInstanceId};

    fn event(seq: u64, kind: EventKind, ts_ms: i64) -> Event {
        Event {
            seq: EventSeq(seq),
            process_instance: ProcessInstanceId("inst".into()),
            process_version: 1,
            kind,
            payload: serde_json::json!({}),
            ts_ms,
        }
    }

    #[test]
    fn success_rate_of_empty_set_is_zero_not_division_by_zero() {
        assert_eq!(compute(&[]).task_success_rate, 0.0);
    }

    #[test]
    fn success_rate_counts_only_finished_instances() {
        let instances = vec![
            (vec![], InstanceOutcome::Finished),
            (vec![], InstanceOutcome::Finished),
            (vec![], InstanceOutcome::AwaitingHuman),
            (vec![], InstanceOutcome::Unresolved),
        ];

        assert_eq!(compute(&instances).task_success_rate, 0.5);
    }

    #[test]
    fn median_latency_of_no_confirmations_is_none() {
        let instances = vec![(vec![], InstanceOutcome::Unresolved)];

        assert_eq!(compute(&instances).median_confirmation_latency_ms, None);
    }

    #[test]
    fn opened_without_resolved_is_not_counted_as_a_latency() {
        let events = vec![event(
            1,
            EventKind::HumanGateOpened { reason: "r".into() },
            1_000,
        )];
        let instances = vec![(events, InstanceOutcome::AwaitingHuman)];

        assert_eq!(compute(&instances).median_confirmation_latency_ms, None);
    }

    #[test]
    fn single_confirmation_pair_gives_its_own_latency_as_the_median() {
        let events = vec![
            event(1, EventKind::HumanGateOpened { reason: "r".into() }, 1_000),
            event(2, EventKind::HumanGateResolved, 4_500),
        ];
        let instances = vec![(events, InstanceOutcome::Finished)];

        assert_eq!(
            compute(&instances).median_confirmation_latency_ms,
            Some(3_500)
        );
    }

    #[test]
    fn latencies_are_aggregated_across_instances_before_taking_the_median() {
        let fast = vec![
            event(1, EventKind::HumanGateOpened { reason: "r".into() }, 0),
            event(2, EventKind::HumanGateResolved, 100),
        ];
        let slow = vec![
            event(1, EventKind::HumanGateOpened { reason: "r".into() }, 0),
            event(2, EventKind::HumanGateResolved, 900),
        ];
        let middle = vec![
            event(1, EventKind::HumanGateOpened { reason: "r".into() }, 0),
            event(2, EventKind::HumanGateResolved, 500),
        ];
        let instances = vec![
            (fast, InstanceOutcome::Finished),
            (slow, InstanceOutcome::Finished),
            (middle, InstanceOutcome::Finished),
        ];

        assert_eq!(
            compute(&instances).median_confirmation_latency_ms,
            Some(500)
        );
    }

    #[test]
    fn multiple_confirmation_rounds_in_one_instance_each_count_separately() {
        let events = vec![
            event(
                1,
                EventKind::HumanGateOpened {
                    reason: "первый".into(),
                },
                0,
            ),
            event(2, EventKind::HumanGateResolved, 200),
            event(
                3,
                EventKind::HumanGateOpened {
                    reason: "второй".into(),
                },
                200,
            ),
            event(4, EventKind::HumanGateResolved, 1_200),
        ];
        let instances = vec![(events, InstanceOutcome::Finished)];

        // Медиана двух наблюдений (200 и 1000) в этой реализации — верхнее
        // из двух средних после сортировки: [200, 1000][2/2] = [1000].
        assert_eq!(
            compute(&instances).median_confirmation_latency_ms,
            Some(1_000)
        );
    }
}
