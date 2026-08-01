//! Доверенный список репозиториев (ROADMAP D5) — реестр строится
//! сверткой журнала событий `EventKind::TrustListChanged`, а не отдельной
//! мутируемой таблицей: список — часть журналируемого локального
//! состояния, тот же журнал, что и остальная память системы
//! (`docs/arch/deployment.md` §4: «Список — часть журналируемого
//! локального состояния (тот же KV-журнал, что и остальная память
//! системы, I5): изменение без соответствующего события — расхождение,
//! обнаруживаемое при загрузке сверткой журнала»).
//!
//! Журнал изменений живёт под синтетическим [`TRUST_LIST_INSTANCE_ID`] —
//! список не принадлежит ни одному конкретному process instance, но
//! `Event`/`EventLog` в этом проекте жёстко типизированы под
//! `ProcessInstanceId`, отдельной глобальной KV-сущности в схеме
//! `berimor-storage` нет (см. `docs/ROADMAP.md` §14, D5 — расследование
//! этой сессии). Использовать тот же журнал под фиксированным
//! идентификатором — не костыль, а буквальное прочтение «тот же
//! KV-журнал» из источника: не заводится параллельная схема хранения
//! ради одной сущности.
//!
//! `berimor_process_engine::state::fold` эти события не обрабатывает
//! (попадают в её `_ => {}`, как `SecurityEvent`/`VersionMigrated`) — эта
//! свёртка отдельная, доменная, не про состояние процесса.

use berimor_types::event::{Event, EventKind, TrustListAction};
use std::collections::HashMap;

/// `ProcessInstanceId`, под которым журналируются изменения доверенного
/// списка — фиксированное, зарезервированное имя (реальный process
/// instance никогда не получит такой id — идентификаторы инстансов
/// генерируются с суффиксом времени/pid, см. `run.rs::new_instance_id`).
pub const TRUST_LIST_INSTANCE_ID: &str = "trust-list";

/// Текущая запись доверенного репозитория — формат `deployment.md` §4.
/// `added_at_ms`/`event_seq` — `added_at`/`event_id` источника: уже даёт
/// их `Event::ts_ms`/`Event::seq`, отдельными полями не дублируются.
#[derive(Debug, Clone, PartialEq)]
pub struct TrustedRepoEntry {
    pub repo: String,
    pub allowed_ref: String,
    pub signer_identity: String,
    pub capability_ceiling: Vec<String>,
    pub added_at_ms: i64,
    pub event_seq: u64,
}

/// Независимое ревью D6 (MAJOR-3): `signer_identity` — SAN-префикс
/// сертификата Fulcio (`ReleaseWorkflowPath` в `verify.rs`), проверка
/// которого — `value.starts_with(prefix)`. Пустая строка удовлетворяет
/// `starts_with("")` для ЛЮБОГО SAN — привязка к конкретному workflow-
/// файлу вырождается в no-op (issuer/repository из `AllOf`-политики
/// по-прежнему пинят GitHub Actions OIDC и владельца/имя репозитория, так
/// что это не полный обход подписи, но реальное ослабление проверки).
/// Используется и в `berimor trust add` (CLI), и в `plugin_install.rs`'s
/// TOFU-пути — одна проверка, не дублируется в каждом месте отдельно.
pub fn is_plausible_signer_identity(value: &str) -> bool {
    let trimmed = value.trim();
    !trimmed.is_empty() && (trimmed.starts_with("https://") || trimmed.starts_with("http://"))
}

/// Сворачивает журнал изменений доверенного списка в текущее состояние.
/// Тот же принцип, что `state::fold` (события применяются по порядку,
/// последнее для данного `repo` побеждает), но своя доменная свёртка —
/// `Added` перезаписывает запись целиком, `Removed` удаляет её. Не
/// `TrustListChanged`-события молча пропускаются (эта функция вызывается
/// на журнале, отфильтрованном `EventLog::replay` под
/// `TRUST_LIST_INSTANCE_ID`, где кроме `TrustListChanged` ничего быть не
/// должно, но падать на неожиданном варианте — излишняя хрупкость для
/// сверток такого рода, тот же выбор, что у `state::fold`'s `_ => {}`).
pub fn fold_trust_list(events: &[Event]) -> HashMap<String, TrustedRepoEntry> {
    let mut list = HashMap::new();
    for event in events {
        let EventKind::TrustListChanged {
            action,
            repo,
            allowed_ref,
            signer_identity,
            capability_ceiling,
        } = &event.kind
        else {
            continue;
        };
        match action {
            TrustListAction::Added => {
                list.insert(
                    repo.clone(),
                    TrustedRepoEntry {
                        repo: repo.clone(),
                        allowed_ref: allowed_ref.clone(),
                        signer_identity: signer_identity.clone(),
                        capability_ceiling: capability_ceiling.clone(),
                        added_at_ms: event.ts_ms,
                        event_seq: event.seq.0,
                    },
                );
            }
            TrustListAction::Removed => {
                list.remove(repo);
            }
        }
    }
    list
}

#[cfg(test)]
mod tests {
    use super::*;
    use berimor_types::event::{EventSeq, ProcessInstanceId};
    use serde_json::Value;

    #[test]
    fn is_plausible_signer_identity_rejects_empty_and_whitespace() {
        assert!(!is_plausible_signer_identity(""));
        assert!(!is_plausible_signer_identity("   "));
        assert!(!is_plausible_signer_identity("\t\n"));
    }

    #[test]
    fn is_plausible_signer_identity_rejects_non_url() {
        assert!(!is_plausible_signer_identity("not-a-url"));
        assert!(!is_plausible_signer_identity("github.com/owner/repo"));
    }

    #[test]
    fn is_plausible_signer_identity_accepts_https_url() {
        assert!(is_plausible_signer_identity(
            "https://github.com/owner/repo/.github/workflows/release.yml@"
        ));
    }

    fn changed(seq: u64, ts_ms: i64, action: TrustListAction, repo: &str) -> Event {
        Event {
            seq: EventSeq(seq),
            process_instance: ProcessInstanceId(TRUST_LIST_INSTANCE_ID.to_string()),
            process_version: 0,
            kind: EventKind::TrustListChanged {
                action,
                repo: repo.to_string(),
                allowed_ref: "v*.*.*".to_string(),
                signer_identity: "https://github.com/owner/repo/.github/workflows/release.yml@"
                    .to_string(),
                capability_ceiling: vec!["net.http".to_string()],
            },
            payload: Value::Null,
            ts_ms,
        }
    }

    #[test]
    fn empty_journal_is_an_empty_list() {
        assert!(fold_trust_list(&[]).is_empty());
    }

    #[test]
    fn added_repo_is_present_with_its_fields() {
        let events = vec![changed(1, 1000, TrustListAction::Added, "owner/repo")];
        let list = fold_trust_list(&events);
        let entry = list.get("owner/repo").unwrap();
        assert_eq!(entry.allowed_ref, "v*.*.*");
        assert_eq!(entry.capability_ceiling, vec!["net.http".to_string()]);
        assert_eq!(entry.added_at_ms, 1000);
        assert_eq!(entry.event_seq, 1);
    }

    #[test]
    fn removed_repo_is_absent() {
        let events = vec![
            changed(1, 1000, TrustListAction::Added, "owner/repo"),
            changed(2, 2000, TrustListAction::Removed, "owner/repo"),
        ];
        assert!(!fold_trust_list(&events).contains_key("owner/repo"));
    }

    #[test]
    fn re_added_after_removal_is_present_again() {
        let events = vec![
            changed(1, 1000, TrustListAction::Added, "owner/repo"),
            changed(2, 2000, TrustListAction::Removed, "owner/repo"),
            changed(3, 3000, TrustListAction::Added, "owner/repo"),
        ];
        let list = fold_trust_list(&events);
        assert_eq!(list.get("owner/repo").unwrap().event_seq, 3);
    }

    #[test]
    fn later_add_overwrites_earlier_fields_for_the_same_repo() {
        let mut second = changed(2, 2000, TrustListAction::Added, "owner/repo");
        let EventKind::TrustListChanged {
            capability_ceiling, ..
        } = &mut second.kind
        else {
            unreachable!()
        };
        *capability_ceiling = vec!["net.http".to_string(), "fs.read".to_string()];

        let events = vec![
            changed(1, 1000, TrustListAction::Added, "owner/repo"),
            second,
        ];
        let list = fold_trust_list(&events);
        assert_eq!(
            list.get("owner/repo").unwrap().capability_ceiling,
            vec!["net.http".to_string(), "fs.read".to_string()]
        );
    }

    #[test]
    fn removing_an_unknown_repo_is_a_noop_not_a_panic() {
        let events = vec![changed(
            1,
            1000,
            TrustListAction::Removed,
            "owner/never-added",
        )];
        assert!(fold_trust_list(&events).is_empty());
    }

    #[test]
    fn two_different_repos_do_not_interfere() {
        let events = vec![
            changed(1, 1000, TrustListAction::Added, "owner/one"),
            changed(2, 2000, TrustListAction::Added, "owner/two"),
            changed(3, 3000, TrustListAction::Removed, "owner/one"),
        ];
        let list = fold_trust_list(&events);
        assert!(!list.contains_key("owner/one"));
        assert!(list.contains_key("owner/two"));
    }
}
