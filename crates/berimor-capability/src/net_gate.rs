//! Сетевой гейт: приватные адреса — только через подтверждение.
//!
//! Источник: `docs/arch/views/network-architecture.md` §3 («Правила
//! гейта»), `docs/arch/security-model.md` §1 (SSRF), §2 (слой L3).
//! ROADMAP: S3.
//!
//! Гейт стоит на границе HTTP-клиента (E5 вызывает его перед каждым
//! соединением). Решение гейта — `ConfirmRequired`, не `Allow`/`Deny`:
//! окончательный вердикт — за слоем режимов подтверждений (S4), который
//! для приватных целей обязан требовать подтверждения в режимах
//! deny/smart/manual (network-architecture.md §3).
//!
//! Блокируемые диапазоны — приватные, loopback, link-local (включая
//! `169.254.169.254` — стандартная цель SSRF на метаданные облака),
//! CGNAT, unspecified; IPv4-mapped IPv6 приводится к IPv4 до проверки,
//! иначе `::ffff:127.0.0.1` был бы бесплатным обходом.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, ToSocketAddrs};

/// Решение сетевого гейта. `Deny` здесь нет: обращение к приватному
/// адресу — не из безусловно запрещённых классов §3.7, а операция,
/// требующая подтверждения (network-architecture.md §3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetworkDecision {
    Allow,
    ConfirmRequired { reason: String },
}

impl NetworkDecision {
    pub fn is_allowed(&self) -> bool {
        matches!(self, Self::Allow)
    }
}

/// Проверка одного адреса — чистая функция, без сети.
pub fn check_ip(ip: IpAddr) -> NetworkDecision {
    if is_private(ip) {
        NetworkDecision::ConfirmRequired {
            reason: format!(
                "приватный/локальный адрес {ip} — требуется подтверждение (сетевой гейт L3)"
            ),
        }
    } else {
        NetworkDecision::Allow
    }
}

/// Проверка цели по имени хоста: литеральный адрес проверяется напрямую,
/// DNS-имя разрешается, и проверяется КАЖДЫЙ полученный адрес — достаточно
/// одного приватного ответа резолвера, чтобы цель ушла на подтверждение.
/// Имя, которое не разрешается, недоказуемо публично — тоже подтверждение
/// (консервативный выбор, как и в deny-статике).
pub fn check_host(host: &str, port: u16) -> NetworkDecision {
    if let Ok(ip) = host.parse::<IpAddr>() {
        return check_ip(ip);
    }
    match (host, port).to_socket_addrs() {
        Ok(addrs) => {
            let addrs: Vec<_> = addrs.collect();
            if addrs.is_empty() {
                return NetworkDecision::ConfirmRequired {
                    reason: format!("имя '{host}' не разрешилось ни в один адрес"),
                };
            }
            for addr in addrs {
                let decision = check_ip(addr.ip());
                if !decision.is_allowed() {
                    return decision;
                }
            }
            NetworkDecision::Allow
        }
        Err(err) => NetworkDecision::ConfirmRequired {
            reason: format!("не удалось разрешить имя '{host}': {err}"),
        },
    }
}

fn is_private(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_private_v4(v4),
        // IPv4-mapped IPv6 (::ffff:a.b.c.d) — приводим к v4 до проверки.
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => is_private_v4(v4),
            None => is_private_v6(v6),
        },
    }
}

fn is_private_v4(ip: Ipv4Addr) -> bool {
    ip.is_private()      // 10/8, 172.16/12, 192.168/16
        || ip.is_loopback()   // 127/8
        || ip.is_link_local() // 169.254/16 — включая метаданные облака
        || ip.is_unspecified() // 0.0.0.0
        || ip.octets()[0] == 100 && (ip.octets()[1] & 0b1100_0000) == 0b0100_0000
    // CGNAT 100.64/10
}

fn is_private_v6(ip: Ipv6Addr) -> bool {
    ip.is_loopback() // ::1
        || ip.is_unspecified() // ::
        || (ip.segments()[0] & 0xfe00) == 0xfc00 // ULA fc00::/7
        || (ip.segments()[0] & 0xffc0) == 0xfe80 // link-local fe80::/10
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("../../../fixtures/golden/security/network-targets.json");

    #[derive(serde::Deserialize)]
    struct Fixture {
        confirm_required: Vec<FixtureCase>,
        allowed: Vec<FixtureCase>,
    }

    #[derive(serde::Deserialize)]
    struct FixtureCase {
        name: String,
        host: String,
    }

    /// Контрактный тест на золотом наборе: каждая приватная цель — на
    /// подтверждение, каждая публичная — свободна.
    #[test]
    fn golden_targets_are_classified_as_documented() {
        let fixture: Fixture = serde_json::from_str(FIXTURE).unwrap();
        for case in &fixture.confirm_required {
            let decision = check_host(&case.host, 443);
            assert!(
                matches!(decision, NetworkDecision::ConfirmRequired { .. }),
                "'{}' ({}) обязана требовать подтверждения, получено {decision:?}",
                case.name,
                case.host
            );
        }
        for case in &fixture.allowed {
            let decision = check_host(&case.host, 443);
            assert!(
                decision.is_allowed(),
                "'{}' ({}) обязана проходить свободно, получено {decision:?}",
                case.name,
                case.host
            );
        }
    }

    #[test]
    fn unresolvable_name_requires_confirmation_not_allow() {
        // .invalid гарантированно не резолвится (RFC 2606) — недоказуемо
        // публичная цель не должна проходить молча.
        let decision = check_host("no-such-host-berimor-test.invalid", 443);
        assert!(matches!(decision, NetworkDecision::ConfirmRequired { .. }));
    }

    #[test]
    fn ipv4_mapped_v6_private_is_caught() {
        let decision = check_ip("::ffff:192.168.0.1".parse().unwrap());
        assert!(matches!(decision, NetworkDecision::ConfirmRequired { .. }));
    }

    #[test]
    fn cgnat_boundary_is_classified() {
        assert!(check_ip("100.63.255.255".parse().unwrap()).is_allowed());
        assert!(matches!(
            check_ip("100.64.0.0".parse().unwrap()),
            NetworkDecision::ConfirmRequired { .. }
        ));
        assert!(matches!(
            check_ip("100.127.255.255".parse().unwrap()),
            NetworkDecision::ConfirmRequired { .. }
        ));
        assert!(check_ip("100.128.0.0".parse().unwrap()).is_allowed());
    }
}
