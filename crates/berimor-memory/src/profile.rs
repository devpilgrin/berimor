//! Изоляция профилей/арендаторов, включая профиль типа `actor`; правило межпрофильного чтения.
//!
//! Источник: `docs/arch/memory-model.md` §5, ADR-0013. ROADMAP: MEM8.
//!
//! ADR-0013 дословно: «Память актора — профиль типа `actor`, та же ось
//! изоляции, что и у профилей пользователей/арендаторов; те же правила
//! маршрутизатора применяются к акторам без отдельного механизма».
//! Отсюда — [`ProfileKind::Actor`] не получает отдельной функции
//! проверки: [`check_access`] — ОДИН код-путь для всех видов профилей
//! (находка ADR-0013: «Отдельный, не связанный с профилями механизм
//! изоляции памяти акторов — отклонено: удваивает поверхность
//! реализации»).
//!
//! Правило по слоям (§5 буквально): рабочая и эпизодическая память
//! приватны профилю по умолчанию (включая профиль-актор — «своя память»
//! актора живёт именно здесь); семантический и процедурный слои по
//! умолчанию ОБЩИЕ на уровне арендатора, не приватны профилю — «процесс
//! может явно объявить актора отдельным профилем и в этих слоях, но это
//! конфигурация, а не поведение по умолчанию» ([`Profile::isolated_layers`]
//! — та самая явная конфигурация). Сверх слой-специфичных умолчаний —
//! межпрофильное чтение только через явное правило маршрутизатора
//! ([`CrossProfileRule`]), никогда неявно.
//!
//! Вне scope этой задачи (упомянуто в §5, но не в названии MEM8):
//! маскировка секретов на записи в семантический/эпизодический слои и
//! опциональный режим персональных данных — оба принадлежат
//! маскировщику (`security-model.md`), отдельному механизму на границе
//! записи, не проверке доступа на чтение, которую реализует этот модуль.

use std::collections::HashSet;

/// Идентификатор профиля — единица изоляции памяти (§5: «память
/// разделена по профилям/арендаторам»).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ProfileId(pub String);

/// Слой памяти — та же четвёрка, что и остальные MEM-задачи (рабочая,
/// эпизодическая, семантическая, процедурная); граф сущностей (MEM7) не
/// самостоятельный слой поверх этой оси изоляции, а надстройка над
/// семантическим (`memory-model.md` §4: «поверх семантического слоя
/// строится граф»), изолируется вместе с ним.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MemoryLayer {
    Working,
    Episodic,
    Semantic,
    Procedural,
}

/// Вид профиля. `Actor` не получает особого обращения в логике доступа
/// (ADR-0013) — вариант существует для читаемости вызывающего кода,
/// который создаёт профиль актора, а не потому что [`check_access`]
/// как-то различает виды.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileKind {
    Tenant,
    User,
    Actor,
}

/// Профиль — единица изоляции плюс принадлежность арендатору, которая
/// определяет слой-специфичное умолчание (§5).
#[derive(Debug, Clone, PartialEq)]
pub struct Profile {
    pub id: ProfileId,
    pub kind: ProfileKind,
    /// Арендатор, к которому принадлежит профиль. У профиля-арендатора
    /// это, естественно, он сам — вызывающий код отвечает за то, чтобы
    /// `tenant == id` в этом случае, здесь не проверяется отдельно
    /// (профиль — то, что уже сконфигурировано, не кандидат на валидацию).
    pub tenant: ProfileId,
    /// Слои, которые владелец явно изолировал от умолчания «общее на
    /// уровне арендатора» — §5: «процесс может явно объявить актора
    /// отдельным профилем... но это конфигурация, а не поведение по
    /// умолчанию». Пустое множество (по умолчанию) — ничего не
    /// изолировано сверх слой-специфичных правил.
    pub isolated_layers: HashSet<MemoryLayer>,
}

/// Явное правило межпрофильного чтения — маршрутизатор (код, не
/// эвристика) объявляет: конкретный профиль-читатель может читать
/// конкретный слой у конкретного профиля-владельца, поверх
/// слой-специфичного умолчания. §5: «межпрофильное чтение — только
/// явным правилом маршрутизатора» — без подходящего правила чтение
/// чужого профиля запрещено всегда, независимо от слоя.
#[derive(Debug, Clone, PartialEq)]
pub struct CrossProfileRule {
    pub reader: ProfileId,
    pub owner: ProfileId,
    pub layer: MemoryLayer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessDecision {
    Allow,
    Deny,
}

/// Рабочая/эпизодическая приватны профилю по умолчанию; семантическая/
/// процедурная общие на уровне арендатора по умолчанию (§5).
fn tenant_shared_by_default(layer: MemoryLayer) -> bool {
    matches!(layer, MemoryLayer::Semantic | MemoryLayer::Procedural)
}

/// Решает, может ли `reader` читать слой `layer` у `owner`.
///
/// Порядок проверок:
/// 1. Тот же профиль — всегда доступ (тривиально, не «межпрофильное»).
/// 2. Слой общий на уровне арендатора по умолчанию, оба профиля одного
///    арендатора, и владелец не изолировал этот слой явно — доступ.
/// 3. Явное правило маршрутизатора для именно этой пары
///    читатель/владелец/слой — доступ.
/// 4. Иначе — отказ.
///
/// Один код-путь для всех видов профилей, включая `ProfileKind::Actor`
/// (ADR-0013) — вызывающий код не обязан (и не должен) ветвиться по
/// виду профиля здесь.
pub fn check_access(
    reader: &Profile,
    owner: &Profile,
    layer: MemoryLayer,
    explicit_rules: &[CrossProfileRule],
) -> AccessDecision {
    if reader.id == owner.id {
        return AccessDecision::Allow;
    }

    if tenant_shared_by_default(layer)
        && reader.tenant == owner.tenant
        && !owner.isolated_layers.contains(&layer)
    {
        return AccessDecision::Allow;
    }

    let explicit_match = explicit_rules
        .iter()
        .any(|rule| rule.reader == reader.id && rule.owner == owner.id && rule.layer == layer);
    if explicit_match {
        return AccessDecision::Allow;
    }

    AccessDecision::Deny
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(id: &str, kind: ProfileKind, tenant: &str) -> Profile {
        Profile {
            id: ProfileId(id.into()),
            kind,
            tenant: ProfileId(tenant.into()),
            isolated_layers: HashSet::new(),
        }
    }

    #[test]
    fn same_profile_always_allowed_on_every_layer() {
        let p = profile("user-1", ProfileKind::User, "tenant-a");
        for layer in [
            MemoryLayer::Working,
            MemoryLayer::Episodic,
            MemoryLayer::Semantic,
            MemoryLayer::Procedural,
        ] {
            assert_eq!(
                check_access(&p, &p, layer, &[]),
                AccessDecision::Allow,
                "профиль обязан всегда читать собственную память, слой {layer:?}"
            );
        }
    }

    #[test]
    fn working_memory_is_private_by_default_even_within_the_same_tenant() {
        let reader = profile("user-1", ProfileKind::User, "tenant-a");
        let owner = profile("user-2", ProfileKind::User, "tenant-a");

        assert_eq!(
            check_access(&reader, &owner, MemoryLayer::Working, &[]),
            AccessDecision::Deny
        );
    }

    #[test]
    fn episodic_memory_is_private_by_default_even_within_the_same_tenant() {
        let reader = profile("user-1", ProfileKind::User, "tenant-a");
        let owner = profile("user-2", ProfileKind::User, "tenant-a");

        assert_eq!(
            check_access(&reader, &owner, MemoryLayer::Episodic, &[]),
            AccessDecision::Deny
        );
    }

    #[test]
    fn semantic_memory_is_shared_by_default_within_the_same_tenant() {
        let reader = profile("user-1", ProfileKind::User, "tenant-a");
        let owner = profile("user-2", ProfileKind::User, "tenant-a");

        assert_eq!(
            check_access(&reader, &owner, MemoryLayer::Semantic, &[]),
            AccessDecision::Allow
        );
    }

    #[test]
    fn procedural_memory_is_shared_by_default_within_the_same_tenant() {
        let reader = profile("user-1", ProfileKind::User, "tenant-a");
        let owner = profile("user-2", ProfileKind::User, "tenant-a");

        assert_eq!(
            check_access(&reader, &owner, MemoryLayer::Procedural, &[]),
            AccessDecision::Allow
        );
    }

    #[test]
    fn semantic_memory_across_different_tenants_is_denied_without_explicit_rule() {
        let reader = profile("user-1", ProfileKind::User, "tenant-a");
        let owner = profile("user-2", ProfileKind::User, "tenant-b");

        assert_eq!(
            check_access(&reader, &owner, MemoryLayer::Semantic, &[]),
            AccessDecision::Deny
        );
    }

    #[test]
    fn explicit_rule_grants_cross_tenant_semantic_access() {
        let reader = profile("user-1", ProfileKind::User, "tenant-a");
        let owner = profile("user-2", ProfileKind::User, "tenant-b");
        let rules = [CrossProfileRule {
            reader: ProfileId("user-1".into()),
            owner: ProfileId("user-2".into()),
            layer: MemoryLayer::Semantic,
        }];

        assert_eq!(
            check_access(&reader, &owner, MemoryLayer::Semantic, &rules),
            AccessDecision::Allow
        );
    }

    #[test]
    fn explicit_rule_grants_access_even_to_layers_private_by_default() {
        let reader = profile("user-1", ProfileKind::User, "tenant-a");
        let owner = profile("user-2", ProfileKind::User, "tenant-a");
        let rules = [CrossProfileRule {
            reader: ProfileId("user-1".into()),
            owner: ProfileId("user-2".into()),
            layer: MemoryLayer::Working,
        }];

        assert_eq!(
            check_access(&reader, &owner, MemoryLayer::Working, &rules),
            AccessDecision::Allow
        );
    }

    #[test]
    fn explicit_rule_does_not_leak_to_other_layers() {
        let reader = profile("user-1", ProfileKind::User, "tenant-a");
        let owner = profile("user-2", ProfileKind::User, "tenant-a");
        let rules = [CrossProfileRule {
            reader: ProfileId("user-1".into()),
            owner: ProfileId("user-2".into()),
            layer: MemoryLayer::Semantic,
        }];

        assert_eq!(
            check_access(&reader, &owner, MemoryLayer::Working, &rules),
            AccessDecision::Deny,
            "правило для Semantic не обязано открывать Working"
        );
    }

    #[test]
    fn explicit_rule_does_not_leak_to_a_different_reader() {
        let other_reader = profile("user-3", ProfileKind::User, "tenant-a");
        let owner = profile("user-2", ProfileKind::User, "tenant-b");
        let rules = [CrossProfileRule {
            reader: ProfileId("user-1".into()),
            owner: ProfileId("user-2".into()),
            layer: MemoryLayer::Semantic,
        }];

        assert_eq!(
            check_access(&other_reader, &owner, MemoryLayer::Semantic, &rules),
            AccessDecision::Deny
        );
    }

    #[test]
    fn actor_profile_follows_the_exact_same_rules_as_any_other_profile_kind() {
        // ADR-0013: один код-путь, без ветвления по ProfileKind.
        let reader = profile("actor-1", ProfileKind::Actor, "tenant-a");
        let owner = profile("user-2", ProfileKind::User, "tenant-a");

        assert_eq!(
            check_access(&reader, &owner, MemoryLayer::Semantic, &[]),
            AccessDecision::Allow,
            "актор внутри того же арендатора по умолчанию делит семантику, как любой профиль"
        );
        assert_eq!(
            check_access(&reader, &owner, MemoryLayer::Working, &[]),
            AccessDecision::Deny,
            "рабочая память актора приватна ему по умолчанию, как у любого профиля"
        );
    }

    #[test]
    fn owner_can_explicitly_isolate_a_normally_shared_layer() {
        let reader = profile("user-1", ProfileKind::User, "tenant-a");
        let mut owner = profile("actor-1", ProfileKind::Actor, "tenant-a");
        owner.isolated_layers.insert(MemoryLayer::Semantic);

        assert_eq!(
            check_access(&reader, &owner, MemoryLayer::Semantic, &[]),
            AccessDecision::Deny,
            "явно изолированный слой не должен доставаться тенант-умолчанием"
        );
    }

    #[test]
    fn isolating_one_layer_does_not_affect_another_layer_default() {
        let reader = profile("user-1", ProfileKind::User, "tenant-a");
        let mut owner = profile("actor-1", ProfileKind::Actor, "tenant-a");
        owner.isolated_layers.insert(MemoryLayer::Semantic);

        assert_eq!(
            check_access(&reader, &owner, MemoryLayer::Procedural, &[]),
            AccessDecision::Allow,
            "изоляция Semantic не обязана трогать Procedural"
        );
    }

    #[test]
    fn explicit_rule_still_grants_access_to_an_isolated_layer() {
        // Изоляция — это отмена ДЕФОЛТА тенант-шаринга, не запрет вообще:
        // явное правило маршрутизатора всё ещё может открыть доступ
        // точечно, поверх изоляции.
        let reader = profile("user-1", ProfileKind::User, "tenant-a");
        let mut owner = profile("actor-1", ProfileKind::Actor, "tenant-a");
        owner.isolated_layers.insert(MemoryLayer::Semantic);
        let rules = [CrossProfileRule {
            reader: ProfileId("user-1".into()),
            owner: ProfileId("actor-1".into()),
            layer: MemoryLayer::Semantic,
        }];

        assert_eq!(
            check_access(&reader, &owner, MemoryLayer::Semantic, &rules),
            AccessDecision::Allow
        );
    }
}
