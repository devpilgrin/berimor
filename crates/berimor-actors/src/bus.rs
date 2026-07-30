//! Шина событий: единственный путь публикации конверта (A2, вторая
//! половина) — проверяет подпись отправителя и ACL топика перед тем, как
//! конверт попадёт в почтовый ящик получателя.
//!
//! Источник: `security-model.md` §4 («шина проверяет ACL топика: компонент
//! не может публиковать события под чужим именем. Источник ACL —
//! статический манифест на диске, который сам компонент переопределить не
//! может»), ADR-0009.
//!
//! ACL переиспользует [`berimor_capability::plugin::PluginManifest`] — тот
//! же механизм, что у плагинов (S6), не отдельный параллельный: doc-
//! комментарий `plugin.rs` заранее называет ACL топика акторов как его
//! вторую точку применения.

use berimor_capability::plugin::{PluginAclError, PluginManifest};
use berimor_storage::{Envelope, MailboxLog, StorageError};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc;

use crate::actor::ActorId;
use crate::signing::{self, SigningKey};

#[derive(Debug, thiserror::Error)]
pub enum PublishError {
    #[error("отправитель '{0}' не зарегистрирован на шине")]
    UnknownSender(ActorId),
    #[error("подпись конверта от '{0}' не проходит проверку")]
    BadSignature(ActorId),
    #[error(transparent)]
    Acl(#[from] PluginAclError),
    /// Конверт уже записан в журнал на момент этой ошибки — не потерян,
    /// подберётся, когда получатель зарегистрируется и восстановится
    /// (`Actor::recover_pending`, A1).
    #[error("получатель '{0}' не зарегистрирован на шине — конверт записан в журнал")]
    UnknownRecipient(ActorId),
    #[error(transparent)]
    Storage(#[from] StorageError),
}

struct Registration {
    key: SigningKey,
    acl: PluginManifest,
    mailbox: mpsc::Sender<Envelope>,
}

/// Единственный путь публикации конверта. Ключ и ACL каждого актора
/// регистрирует ХОСТ при создании актора — сам актор не может ни то, ни
/// другое поменять (тот же принцип, что «источник ACL... сам компонент
/// переопределить не может» у [`PluginManifest`]).
pub struct EventBus<L: MailboxLog + Send + Sync> {
    log: Arc<L>,
    registry: HashMap<ActorId, Registration>,
}

impl<L: MailboxLog + Send + Sync> EventBus<L> {
    pub fn new(log: Arc<L>) -> Self {
        Self {
            log,
            registry: HashMap::new(),
        }
    }

    /// Регистрирует актора: ключ, которым проверяется его подпись, ACL,
    /// ограничивающий его топики, и канал его почтового ящика.
    /// Перезаписывает предыдущую регистрацию того же `id`, если была —
    /// вызывающий код (хост) отвечает за то, чтобы не звать это дважды по
    /// ошибке с другим ключом для того же ещё активного актора.
    pub fn register_actor(
        &mut self,
        id: ActorId,
        key: SigningKey,
        acl: PluginManifest,
        mailbox: mpsc::Sender<Envelope>,
    ) {
        self.registry.insert(id, Registration { key, acl, mailbox });
    }

    /// Публикует конверт: проверяет личность отправителя (подпись), затем
    /// его право на топик (ACL), затем журналирует, затем пробует
    /// доставить. В этом порядке — неизвестный отправитель не проходит
    /// ACL-проверку раньше проверки личности, а конверт от подтверждённого
    /// отправителя попадает в журнал независимо от того, готов ли получатель
    /// его принять прямо сейчас.
    pub async fn publish(&self, envelope: Envelope, signature: &[u8]) -> Result<(), PublishError> {
        let sender = self
            .registry
            .get(&envelope.from)
            .ok_or_else(|| PublishError::UnknownSender(envelope.from.clone()))?;

        if !signing::verify(&sender.key, &envelope, signature) {
            return Err(PublishError::BadSignature(envelope.from.clone()));
        }

        sender.acl.check_event(&envelope.topic)?;

        self.log.persist_envelope(&envelope)?;

        let Some(recipient) = self.registry.get(&envelope.to) else {
            return Err(PublishError::UnknownRecipient(envelope.to.clone()));
        };
        let mailbox = recipient.mailbox.clone();
        // Best-effort доставка: конверт уже в журнале, недоставленное
        // подбирается `Actor::recover_pending` — переполненный/закрытый
        // канал получателя не теряет сообщение, только откладывает.
        let _ = mailbox.send(envelope).await;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use berimor_storage::{EnvelopeId, SqliteEventLog};
    use serde_json::json;

    fn manifest(name: &str, allowed_events: &[&str]) -> PluginManifest {
        PluginManifest {
            name: name.into(),
            allowed_events: allowed_events.iter().map(|s| s.to_string()).collect(),
            allowed_secrets: vec![],
            capability_ceiling: vec![],
        }
    }

    fn envelope(from: &str, to: &str, topic: &str) -> Envelope {
        Envelope {
            id: EnvelopeId("e-1".into()),
            from: from.into(),
            to: to.into(),
            topic: topic.into(),
            payload: json!({"n": 1}),
        }
    }

    #[tokio::test]
    async fn publish_delivers_correctly_signed_envelope_within_acl() {
        let log = Arc::new(SqliteEventLog::open_in_memory().unwrap());
        let mut bus = EventBus::new(log.clone());
        let key_a = SigningKey::generate();
        let (tx_a, _rx_a) = mpsc::channel(8);
        let (tx_b, mut rx_b) = mpsc::channel(8);
        bus.register_actor(
            "actor-a".into(),
            key_a.clone(),
            manifest("actor-a", &["t"]),
            tx_a,
        );
        bus.register_actor(
            "actor-b".into(),
            SigningKey::generate(),
            manifest("actor-b", &[]),
            tx_b,
        );

        let envelope = envelope("actor-a", "actor-b", "t");
        let signature = signing::sign(&key_a, &envelope);

        bus.publish(envelope.clone(), &signature).await.unwrap();

        let received = rx_b.recv().await.unwrap();
        assert_eq!(received, envelope);
    }

    #[tokio::test]
    async fn publish_from_unregistered_sender_is_rejected() {
        let log = Arc::new(SqliteEventLog::open_in_memory().unwrap());
        let bus: EventBus<SqliteEventLog> = EventBus::new(log);
        let envelope = envelope("ghost", "actor-b", "t");
        let signature = signing::sign(&SigningKey::generate(), &envelope);

        let result = bus.publish(envelope, &signature).await;

        assert!(matches!(result, Err(PublishError::UnknownSender(id)) if id == "ghost"));
    }

    #[tokio::test]
    async fn publish_with_wrong_signature_is_rejected() {
        let log = Arc::new(SqliteEventLog::open_in_memory().unwrap());
        let mut bus = EventBus::new(log);
        let (tx_a, _rx_a) = mpsc::channel(8);
        bus.register_actor(
            "actor-a".into(),
            SigningKey::generate(),
            manifest("actor-a", &["t"]),
            tx_a,
        );
        let envelope = envelope("actor-a", "actor-b", "t");
        let wrong_signature = signing::sign(&SigningKey::generate(), &envelope);

        let result = bus.publish(envelope, &wrong_signature).await;

        assert!(matches!(result, Err(PublishError::BadSignature(id)) if id == "actor-a"));
    }

    #[tokio::test]
    async fn publish_cannot_impersonate_another_registered_sender() {
        // "Компонент не может публиковать события под чужим именем" —
        // актор-b подписывает конверт своим ключом, но заявляет от=actor-a.
        let log = Arc::new(SqliteEventLog::open_in_memory().unwrap());
        let mut bus = EventBus::new(log);
        let (tx_a, _rx_a) = mpsc::channel(8);
        let (tx_b, _rx_b) = mpsc::channel(8);
        bus.register_actor(
            "actor-a".into(),
            SigningKey::generate(),
            manifest("actor-a", &["t"]),
            tx_a,
        );
        let key_b = SigningKey::generate();
        bus.register_actor(
            "actor-b".into(),
            key_b.clone(),
            manifest("actor-b", &["t"]),
            tx_b,
        );

        let forged = envelope("actor-a", "actor-b", "t");
        let signature_by_b = signing::sign(&key_b, &forged);

        let result = bus.publish(forged, &signature_by_b).await;

        assert!(matches!(result, Err(PublishError::BadSignature(id)) if id == "actor-a"));
    }

    #[tokio::test]
    async fn publish_to_topic_outside_acl_is_rejected() {
        let log = Arc::new(SqliteEventLog::open_in_memory().unwrap());
        let mut bus = EventBus::new(log);
        let key_a = SigningKey::generate();
        let (tx_a, _rx_a) = mpsc::channel(8);
        bus.register_actor(
            "actor-a".into(),
            key_a.clone(),
            manifest("actor-a", &["allowed-topic"]),
            tx_a,
        );
        let envelope = envelope("actor-a", "actor-b", "forbidden-topic");
        let signature = signing::sign(&key_a, &envelope);

        let result = bus.publish(envelope, &signature).await;

        assert!(matches!(result, Err(PublishError::Acl(_))));
    }

    #[tokio::test]
    async fn publish_with_empty_acl_denies_every_topic_fail_closed() {
        let log = Arc::new(SqliteEventLog::open_in_memory().unwrap());
        let mut bus = EventBus::new(log);
        let key_a = SigningKey::generate();
        let (tx_a, _rx_a) = mpsc::channel(8);
        bus.register_actor(
            "actor-a".into(),
            key_a.clone(),
            manifest("actor-a", &[]),
            tx_a,
        );
        let envelope = envelope("actor-a", "actor-b", "anything");
        let signature = signing::sign(&key_a, &envelope);

        let result = bus.publish(envelope, &signature).await;

        assert!(matches!(result, Err(PublishError::Acl(_))));
    }

    #[tokio::test]
    async fn publish_to_unregistered_recipient_still_journals_the_envelope() {
        let log = Arc::new(SqliteEventLog::open_in_memory().unwrap());
        let mut bus = EventBus::new(log.clone());
        let key_a = SigningKey::generate();
        let (tx_a, _rx_a) = mpsc::channel(8);
        bus.register_actor(
            "actor-a".into(),
            key_a.clone(),
            manifest("actor-a", &["t"]),
            tx_a,
        );
        let envelope = envelope("actor-a", "actor-b-not-registered-yet", "t");
        let signature = signing::sign(&key_a, &envelope);

        let result = bus.publish(envelope, &signature).await;

        assert!(
            matches!(result, Err(PublishError::UnknownRecipient(id)) if id == "actor-b-not-registered-yet")
        );
        let undelivered = log.undelivered_for("actor-b-not-registered-yet").unwrap();
        assert_eq!(
            undelivered.len(),
            1,
            "конверт должен быть в журнале даже если получатель ещё не зарегистрирован"
        );
    }

    #[tokio::test]
    async fn signature_valid_for_one_envelope_does_not_verify_a_different_one() {
        let log = Arc::new(SqliteEventLog::open_in_memory().unwrap());
        let mut bus = EventBus::new(log);
        let key_a = SigningKey::generate();
        let (tx_a, _rx_a) = mpsc::channel(8);
        let (tx_b, _rx_b) = mpsc::channel(8);
        bus.register_actor(
            "actor-a".into(),
            key_a.clone(),
            manifest("actor-a", &["t"]),
            tx_a,
        );
        bus.register_actor(
            "actor-b".into(),
            SigningKey::generate(),
            manifest("actor-b", &[]),
            tx_b,
        );

        let original = envelope("actor-a", "actor-b", "t");
        let signature = signing::sign(&key_a, &original);
        let mut different_payload = original;
        different_payload.payload = json!({"n": 999});

        let result = bus.publish(different_payload, &signature).await;

        assert!(matches!(result, Err(PublishError::BadSignature(_))));
    }
}
