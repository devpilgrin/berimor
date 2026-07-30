//! Актор: задача + почтовый ящик + профиль памяти; аварийная заморозка.
//!
//! Источник: `ideal-agent-architecture.md` §3.8, `security-model.md` §4, ADR-0009.
//! ROADMAP: A1 (модель актора) · A6 (аварийная заморозка).
//!
//! Подпись конвертов и проверка ACL топика (A2) — в [`crate::bus`]:
//! конверт попадает в `mailbox_rx` этого актора только через
//! `EventBus::publish`, уже проверенным. [`berimor_storage::Envelope`]
//! здесь несёт адресацию — то, что уже прошло проверку бортом раньше.

use berimor_storage::{Envelope, MailboxLog};
use std::sync::Arc;
use tokio::sync::{mpsc, watch};

/// Идентификатор актора — совпадает с идентификатором профиля памяти
/// типа `actor` (ADR-0013, `memory-model.md` §5): один узел на этой оси
/// изоляции, не два.
pub type ActorId = String;

#[derive(Debug, thiserror::Error)]
pub enum HandlerError {
    #[error("обработка конверта провалилась: {0}")]
    Failed(String),
}

/// Обработка одного конверта. Актор сам не решает, ЧТО делать с
/// сообщением (это прогон процесса конкретного домена) — только КОГДА и
/// КАК его получить и подтвердить доставку. То же разделение
/// ответственности, что у `StepExecutor` (P3, Process Engine): общая
/// механика здесь, конкретное поведение — у вызывающего кода.
pub trait EnvelopeHandler: Send + Sync {
    fn handle(&self, envelope: &Envelope) -> Result<(), HandlerError>;
}

/// Переключатель аварийной заморозки (A6, `security-model.md` §4: «доля
/// секунды», «активные задачи не теряются»). `tokio::sync::watch` —
/// широковещательный сигнал без дополнительной зависимости: изменение
/// видят все подписчики `subscribe()` за один `.changed()`.
#[derive(Clone)]
pub struct FreezeSwitch {
    tx: Arc<watch::Sender<bool>>,
}

impl FreezeSwitch {
    /// Возвращает переключатель и приёмник для первого актора; каждый
    /// следующий актор получает приёмник через [`FreezeSwitch::subscribe`].
    pub fn new() -> (Self, watch::Receiver<bool>) {
        let (tx, rx) = watch::channel(false);
        (Self { tx: Arc::new(tx) }, rx)
    }

    pub fn subscribe(&self) -> watch::Receiver<bool> {
        self.tx.subscribe()
    }

    /// Замораживает всех подписчиков. Рассылка `watch`-канала мгновенна
    /// и не буферизует историю (только последнее значение) — но актор
    /// реагирует между обработкой конвертов, не прерывая обработку
    /// текущего: `EnvelopeHandler::handle` синхронен и не отменяем этим
    /// циклом. Конверт, который актор не успел ВЗЯТЬ из ящика до
    /// заморозки, остаётся недоставленным в журнале — «активные задачи
    /// не теряются, а ставятся обратно в очередь» реализуется тем, что
    /// requeue-шага не существует отдельно: недоставленное и так в
    /// очереди, подбирается [`Actor::recover_pending`] после разморозки.
    pub fn freeze_all(&self) {
        let _ = self.tx.send(true);
    }

    pub fn unfreeze_all(&self) {
        let _ = self.tx.send(false);
    }
}

/// Актор: цикл получения из почтового ящика + вызов обработчика +
/// подтверждение доставки ПОСЛЕ успешной обработки. `run()` — задача
/// tokio (А1: «актор = задача tokio + почтовый ящик»).
pub struct Actor<L: MailboxLog + Send + Sync> {
    pub id: ActorId,
    /// Профиль памяти актора (ADR-0013/MEM8) — идентификатор профиля
    /// типа `actor`, привязанный к этому актору. Сама изоляция
    /// (`berimor_memory::profile::check_access`) применяется вызывающим
    /// кодом при обращении к слоям памяти, не этим типом — актор не
    /// зависит от `berimor-memory` ради одной строки-метки.
    pub memory_profile: String,
    log: Arc<L>,
    mailbox_rx: mpsc::Receiver<Envelope>,
    freeze_rx: watch::Receiver<bool>,
    handler: Arc<dyn EnvelopeHandler>,
}

impl<L: MailboxLog + Send + Sync + 'static> Actor<L> {
    pub fn new(
        id: ActorId,
        memory_profile: String,
        log: Arc<L>,
        mailbox_rx: mpsc::Receiver<Envelope>,
        freeze_rx: watch::Receiver<bool>,
        handler: Arc<dyn EnvelopeHandler>,
    ) -> Self {
        Self {
            id,
            memory_profile,
            log,
            mailbox_rx,
            freeze_rx,
            handler,
        }
    }

    /// Недоставленные конверты из журнала на момент старта — то, что
    /// осталось после сбоя/заморозки предыдущего запуска (тот же приём
    /// восстановления, что `engine::recover` у Process Engine, P3).
    pub fn recover_pending(&self) -> Result<Vec<Envelope>, berimor_storage::StorageError> {
        self.log.undelivered_for(&self.id)
    }

    /// Основной цикл: получить конверт → обработать → подтвердить
    /// доставку. Останавливается, когда почтовый ящик закрыт ИЛИ пришёл
    /// сигнал заморозки — в обоих случаях выходит без паники.
    /// `biased` — сигнал заморозки проверяется раньше следующего
    /// сообщения на каждой итерации: полученный, но ещё не
    /// обработанный сигнал не должен ждать произвольно долго за
    /// случайным выбором `select!`.
    pub async fn run(&mut self) {
        loop {
            tokio::select! {
                biased;
                changed = self.freeze_rx.changed() => {
                    if changed.is_err() {
                        // Отправитель уничтожен — не повод падать, просто
                        // больше некому морозить/размораживать.
                        continue;
                    }
                    if *self.freeze_rx.borrow() {
                        return;
                    }
                }
                envelope = self.mailbox_rx.recv() => {
                    let Some(envelope) = envelope else {
                        return;
                    };
                    if self.handler.handle(&envelope).is_ok() {
                        let _ = self.log.mark_delivered(&envelope.id);
                    }
                    // Провал обработки: конверт остаётся недоставленным в
                    // журнале — эскалация по числу неудач решается
                    // диспетчером (A3), не этим циклом.
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use berimor_storage::{EnvelopeId, SqliteEventLog};
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn envelope(id: &str) -> Envelope {
        Envelope {
            id: EnvelopeId(id.into()),
            from: "actor-a".into(),
            to: "actor-b".into(),
            topic: "t".into(),
            payload: json!({"n": 1}),
        }
    }

    struct CountingHandler {
        handled: AtomicUsize,
        fail_on: Option<String>,
    }

    impl EnvelopeHandler for CountingHandler {
        fn handle(&self, envelope: &Envelope) -> Result<(), HandlerError> {
            self.handled.fetch_add(1, Ordering::SeqCst);
            if self.fail_on.as_deref() == Some(envelope.id.0.as_str()) {
                return Err(HandlerError::Failed("намеренный сбой теста".into()));
            }
            Ok(())
        }
    }

    #[tokio::test]
    async fn processed_envelope_is_marked_delivered() {
        let log = Arc::new(SqliteEventLog::open_in_memory().unwrap());
        log.persist_envelope(&envelope("e-1")).unwrap();

        let (mailbox_tx, mailbox_rx) = mpsc::channel(8);
        let (_freeze, freeze_rx) = FreezeSwitch::new();
        let handler = Arc::new(CountingHandler {
            handled: AtomicUsize::new(0),
            fail_on: None,
        });
        let mut actor = Actor::new(
            "actor-b".into(),
            "profile-actor-b".into(),
            log.clone(),
            mailbox_rx,
            freeze_rx,
            handler,
        );

        mailbox_tx.send(envelope("e-1")).await.unwrap();
        drop(mailbox_tx); // закрывает канал — run() завершится сам

        actor.run().await;

        assert!(log.undelivered_for("actor-b").unwrap().is_empty());
    }

    #[tokio::test]
    async fn failed_handler_leaves_envelope_undelivered() {
        let log = Arc::new(SqliteEventLog::open_in_memory().unwrap());
        log.persist_envelope(&envelope("e-1")).unwrap();

        let (mailbox_tx, mailbox_rx) = mpsc::channel(8);
        let (_freeze, freeze_rx) = FreezeSwitch::new();
        let handler = Arc::new(CountingHandler {
            handled: AtomicUsize::new(0),
            fail_on: Some("e-1".into()),
        });
        let mut actor = Actor::new(
            "actor-b".into(),
            "profile-actor-b".into(),
            log.clone(),
            mailbox_rx,
            freeze_rx,
            handler,
        );

        mailbox_tx.send(envelope("e-1")).await.unwrap();
        drop(mailbox_tx);

        actor.run().await;

        let undelivered = log.undelivered_for("actor-b").unwrap();
        assert_eq!(
            undelivered.len(),
            1,
            "провал обработки не должен подтверждать доставку"
        );
    }

    #[tokio::test]
    async fn recover_pending_returns_undelivered_envelopes_from_a_previous_run() {
        let log = Arc::new(SqliteEventLog::open_in_memory().unwrap());
        log.persist_envelope(&envelope("e-1")).unwrap();

        let (_mailbox_tx, mailbox_rx) = mpsc::channel(8);
        let (_freeze, freeze_rx) = FreezeSwitch::new();
        let handler = Arc::new(CountingHandler {
            handled: AtomicUsize::new(0),
            fail_on: None,
        });
        let actor = Actor::new(
            "actor-b".into(),
            "profile-actor-b".into(),
            log,
            mailbox_rx,
            freeze_rx,
            handler,
        );

        let pending = actor.recover_pending().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, EnvelopeId("e-1".into()));
    }

    #[tokio::test]
    async fn freeze_signal_stops_the_actor_loop_without_panicking() {
        let log = Arc::new(SqliteEventLog::open_in_memory().unwrap());
        let (_mailbox_tx, mailbox_rx) = mpsc::channel::<Envelope>(8);
        let (freeze, freeze_rx) = FreezeSwitch::new();
        let handler = Arc::new(CountingHandler {
            handled: AtomicUsize::new(0),
            fail_on: None,
        });
        let mut actor = Actor::new(
            "actor-b".into(),
            "profile-actor-b".into(),
            log,
            mailbox_rx,
            freeze_rx,
            handler,
        );

        freeze.freeze_all();
        // Почтовый ящик пуст и открыт (mailbox_tx жив) — без сигнала
        // заморозки run() висел бы вечно; тест проходит только если
        // заморозка реально останавливает цикл.
        actor.run().await;
    }

    #[tokio::test]
    async fn mailbox_closed_without_freeze_stops_the_loop_cleanly() {
        let log = Arc::new(SqliteEventLog::open_in_memory().unwrap());
        let (mailbox_tx, mailbox_rx) = mpsc::channel::<Envelope>(8);
        let (_freeze, freeze_rx) = FreezeSwitch::new();
        let handler = Arc::new(CountingHandler {
            handled: AtomicUsize::new(0),
            fail_on: None,
        });
        let mut actor = Actor::new(
            "actor-b".into(),
            "profile-actor-b".into(),
            log,
            mailbox_rx,
            freeze_rx,
            handler,
        );

        drop(mailbox_tx);
        actor.run().await;
    }
}
