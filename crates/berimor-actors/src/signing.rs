//! Подпись конвертов отправителем (A2, первая половина).
//!
//! Источник: `security-model.md` §4 («Конверты акторов подписаны
//! отправителем»), ADR-0009.
//!
//! Ключ выдаёт ХОСТ при регистрации актора на шине
//! ([`crate::bus::EventBus::register_actor`]) — единственный публичный
//! конструктор вне тестов — [`SigningKey::generate`]. Актор не выбирает
//! свой ключ и физически не может подписать конверт от чужого имени: у
//! него просто нет чужого ключа. Проверка подписи и ACL топика — в
//! [`crate::bus`], этот модуль — только примитив подписи/проверки.

use berimor_storage::Envelope;
use hmac::{Hmac, KeyInit, Mac};
use rand::{rng, RngExt};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

const KEY_LEN: usize = 32;

/// Ключ подписи одного актора. Не сериализуется и не выводится в Debug —
/// секрет, а не диагностика (тот же принцип, что `berimor_secrets`).
#[derive(Clone)]
pub struct SigningKey([u8; KEY_LEN]);

impl std::fmt::Debug for SigningKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("SigningKey").field(&"<redacted>").finish()
    }
}

impl SigningKey {
    /// Криптографически случайный ключ.
    pub fn generate() -> Self {
        let mut bytes = [0u8; KEY_LEN];
        rng().fill(&mut bytes);
        Self(bytes)
    }

    #[cfg(test)]
    pub fn from_bytes(bytes: [u8; KEY_LEN]) -> Self {
        Self(bytes)
    }
}

pub type Signature = Vec<u8>;

/// Детерминированные байты конверта для MAC: `serde_json::Value::Object`
/// в этом воркспейсе всегда сериализуется по отсортированным ключам
/// (`serde_json` без фичи `preserve_order` использует `BTreeMap`) — порядок
/// полей в `json!` ниже не влияет на итоговые байты.
fn canonical_bytes(envelope: &Envelope) -> Vec<u8> {
    let value = serde_json::json!({
        "id": envelope.id.0,
        "from": envelope.from,
        "to": envelope.to,
        "topic": envelope.topic,
        "payload": envelope.payload,
    });
    serde_json::to_vec(&value).expect("json! строит только сериализуемые значения")
}

/// Подписывает конверт ключом отправителя.
pub fn sign(key: &SigningKey, envelope: &Envelope) -> Signature {
    let mut mac =
        HmacSha256::new_from_slice(&key.0).expect("HMAC-SHA256 принимает ключ любой длины");
    mac.update(&canonical_bytes(envelope));
    mac.finalize().into_bytes().to_vec()
}

/// Проверяет подпись константным по времени сравнением
/// (`Mac::verify_slice`), не побайтовым `==`, которое утекало бы через
/// тайминги.
pub fn verify(key: &SigningKey, envelope: &Envelope, signature: &[u8]) -> bool {
    let mut mac =
        HmacSha256::new_from_slice(&key.0).expect("HMAC-SHA256 принимает ключ любой длины");
    mac.update(&canonical_bytes(envelope));
    mac.verify_slice(signature).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use berimor_storage::EnvelopeId;
    use serde_json::json;

    fn envelope(from: &str, topic: &str) -> Envelope {
        Envelope {
            id: EnvelopeId("e-1".into()),
            from: from.into(),
            to: "actor-b".into(),
            topic: topic.into(),
            payload: json!({"n": 1}),
        }
    }

    #[test]
    fn signature_verifies_with_the_same_key_and_envelope() {
        let key = SigningKey::from_bytes([7; KEY_LEN]);
        let envelope = envelope("actor-a", "t");

        let signature = sign(&key, &envelope);

        assert!(verify(&key, &envelope, &signature));
    }

    #[test]
    fn signature_fails_with_a_different_key() {
        let signer = SigningKey::from_bytes([7; KEY_LEN]);
        let other = SigningKey::from_bytes([9; KEY_LEN]);
        let envelope = envelope("actor-a", "t");

        let signature = sign(&signer, &envelope);

        assert!(!verify(&other, &envelope, &signature));
    }

    #[test]
    fn signature_fails_if_envelope_was_tampered_with_after_signing() {
        let key = SigningKey::from_bytes([7; KEY_LEN]);
        let original = envelope("actor-a", "t");
        let signature = sign(&key, &original);

        let mut tampered = original;
        tampered.topic = "another-topic".into();

        assert!(!verify(&key, &tampered, &signature));
    }

    #[test]
    fn signature_fails_if_sender_field_is_forged_after_signing() {
        let key = SigningKey::from_bytes([7; KEY_LEN]);
        let original = envelope("actor-a", "t");
        let signature = sign(&key, &original);

        let mut forged = original;
        forged.from = "actor-victim".into();

        assert!(
            !verify(&key, &forged, &signature),
            "изменение поля from после подписи должно ломать проверку"
        );
    }

    #[test]
    fn two_generated_keys_are_not_equal() {
        // Не строгое криптографическое доказательство энтропии, но ловит
        // грубые ошибки вроде константного/нулевого ключа по умолчанию.
        let a = SigningKey::generate();
        let b = SigningKey::generate();
        assert_ne!(a.0, b.0);
    }
}
