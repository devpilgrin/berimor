//! Проверка подписи платформенного артефакта — keyless sigstore/cosign.
//!
//! Источник: `docs/arch/deployment.md` §7, §9; ADR-0026. ROADMAP: D2.
//!
//! Артефакт подписан в CI (`.github/workflows/release.yml`) через
//! `cosign sign-blob --bundle` — keyless: Fulcio выдаёт короткоживущий
//! сертификат по OIDC-токену конкретного workflow-запуска, приватного
//! ключа нигде нет. Здесь эта подпись проверяется целиком в Rust, без
//! зависимости от установленного на машине пользователя `cosign` —
//! bootstrap (`bootstrap/src/verify.ts`) делегирует сюда РОВНО ЧТОБЫ не
//! переизобретать проверку на другом языке (ADR-0025), а не чтобы затем
//! требовать ещё один внешний бинарник.
//!
//! **Зашитый якорь доверия.** Идентичность подписанта (repo + имя
//! workflow + OIDC issuer) — константы ниже, не общий доверенный список
//! (`deployment.md` §4, D5 — отдельная, ещё не реализованная задача).
//! Это осознанно узкая, а не временная граница: `deployment.md` §4 прямо
//! называет «встроенные значения по умолчанию в bootstrap-сборке» частью
//! формата доверенного списка — для СВОЕГО СОБСТВЕННОГО репозитория это
//! и есть тот встроенный якорь; общий формат (событие + подтверждение
//! для ЧУЖИХ репозиториев — плагинов) — предмет D5, к проверке
//! собственных релизных артефактов отношения не имеющий.
//!
//! **Честный пробел версии крейта.** `sigstore` 0.14.0 сам отмечает
//! (см. исходники `bundle::verify::verifier`, комментарии с пометкой
//! `TODO(tnytown)`), что верификация Merkle-inclusion-proof и Signed
//! Entry Timestamp Rekor ещё не реализована — верификация здесь честно
//! наследует это ограничение библиотеки, не маскирует его. Используется
//! офлайн-режим (`offline: true`): сверка идёт по записи транспарентного
//! лога, ВСТРОЕННОЙ в сам бандл, без обращения к живому Rekor API — не
//! слабее по факту (те же проверки crate всё равно не делает online), но
//! не требует сетевой доступности Rekor на машине пользователя для этого
//! конкретного шага (доступность Fulcio/CTFE trust root по TUF всё равно
//! нужна — тот же сетевой шаг, что уже выполняет `download.ts` для самого
//! артефакта).

use std::fs;
use std::io;
use std::path::Path;

use sigstore::bundle::verify::blocking::Verifier;
use sigstore::bundle::verify::policy::{
    AllOf, GitHubWorkflowName, GitHubWorkflowRepository, OIDCIssuer, SingleX509ExtPolicy,
    VerificationPolicy,
};
use sigstore::bundle::verify::VerificationError;
use sigstore::bundle::Bundle;
use sigstore::errors::SigstoreError;

const GITHUB_OIDC_ISSUER: &str = "https://token.actions.githubusercontent.com";
const RELEASE_REPOSITORY: &str = "devpilgrin/berimor";
/// Значение поля `name:` в `.github/workflows/release.yml` — не путь к файлу
/// (для пути к файлу пришлось бы разбирать SAN-идентичность сертификата
/// вручную; имя workflow — уже есть отдельным X.509-расширением Fulcio,
/// не зависит от того, какой ref/событие вызвали прогон).
const RELEASE_WORKFLOW_NAME: &str = "Release";

#[derive(Debug, thiserror::Error)]
pub enum VerifyError {
    #[error("не удалось прочитать файл бандла подписи `{path}`: {source}")]
    ReadBundle { path: String, source: io::Error },
    #[error("бандл подписи `{path}` — не валидный JSON: {source}")]
    ParseBundle {
        path: String,
        source: serde_json::Error,
    },
    #[error("не удалось открыть артефакт `{path}`: {source}")]
    OpenArtifact { path: String, source: io::Error },
    #[error("не удалось построить доверенный корень sigstore: {0}")]
    TrustRoot(#[source] SigstoreError),
    #[error("подпись не прошла верификацию: {0}")]
    Verification(#[source] VerificationError),
}

/// Путь к бандлу подписи для артефакта — соглашение имени из
/// `.github/workflows/release.yml`: `<artifact>.sigstore.json` рядом с
/// самим артефактом.
pub fn bundle_path_for(artifact_path: &Path) -> std::path::PathBuf {
    let mut name = artifact_path.as_os_str().to_owned();
    name.push(".sigstore.json");
    std::path::PathBuf::from(name)
}

/// Проверяет артефакт против бандла подписи, лежащего рядом (см.
/// [`bundle_path_for`]). Возвращает `Err`, если бандла нет, он повреждён,
/// либо подпись/сертификат/идентичность подписанта не проходят
/// верификацию — молчаливого успеха при отсутствии бандла нет: I6
/// («ошибка верификации не преодолевается подтверждением») распространяется
/// и на «нечего проверять».
pub fn verify_artifact(artifact_path: &Path) -> Result<(), VerifyError> {
    let bundle_path = bundle_path_for(artifact_path);

    let bundle_json =
        fs::read_to_string(&bundle_path).map_err(|source| VerifyError::ReadBundle {
            path: bundle_path.display().to_string(),
            source,
        })?;
    let bundle: Bundle =
        serde_json::from_str(&bundle_json).map_err(|source| VerifyError::ParseBundle {
            path: bundle_path.display().to_string(),
            source,
        })?;

    let artifact = fs::File::open(artifact_path).map_err(|source| VerifyError::OpenArtifact {
        path: artifact_path.display().to_string(),
        source,
    })?;

    let verifier = Verifier::production().map_err(VerifyError::TrustRoot)?;

    let issuer = OIDCIssuer::new(GITHUB_OIDC_ISSUER);
    let repository = GitHubWorkflowRepository::new(RELEASE_REPOSITORY);
    let workflow_name = GitHubWorkflowName::new(RELEASE_WORKFLOW_NAME);
    let policy = AllOf::new([
        &issuer as &dyn VerificationPolicy,
        &repository as &dyn VerificationPolicy,
        &workflow_name as &dyn VerificationPolicy,
    ])
    .expect("список политик статически непуст");

    verifier
        .verify(artifact, bundle, &policy, true)
        .map_err(VerifyError::Verification)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixtures_dir() -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/golden/signing")
    }

    #[test]
    fn valid_fixture_verifies() {
        let artifact = fixtures_dir().join("RELEASE.txt");
        let result = verify_artifact(&artifact);
        assert!(result.is_ok(), "ожидался успех, получено: {result:?}");
    }

    #[test]
    fn missing_bundle_is_rejected() {
        let dir = tempdir();
        let artifact = dir.path().join("no-bundle.bin");
        fs::write(&artifact, b"whatever").unwrap();

        let err = verify_artifact(&artifact).expect_err("должен быть отказ без бандла");
        assert!(matches!(err, VerifyError::ReadBundle { .. }));
    }

    #[test]
    fn corrupted_bundle_json_is_rejected() {
        let dir = tempdir();
        let artifact = dir.path().join("bad-json.bin");
        fs::write(&artifact, b"whatever").unwrap();
        fs::write(bundle_path_for(&artifact), b"{ not json").unwrap();

        let err = verify_artifact(&artifact).expect_err("должен быть отказ на битом JSON");
        assert!(matches!(err, VerifyError::ParseBundle { .. }));
    }

    #[test]
    fn tampered_artifact_content_fails_signature_check() {
        let valid_artifact = fixtures_dir().join("RELEASE.txt");
        let valid_bundle = fixtures_dir().join("RELEASE.txt.sigstore.json");

        let dir = tempdir();
        let artifact = dir.path().join("RELEASE.txt");
        // Тот же путь к бандлу, что и оригинал (`bundle_path_for` — по
        // соглашению имени), но содержимое артефакта изменено — digest
        // внутри бандла больше не соответствует, подпись не сойдётся.
        let mut original = fs::read(&valid_artifact).unwrap();
        original.push(b'!');
        fs::write(&artifact, &original).unwrap();
        fs::copy(&valid_bundle, bundle_path_for(&artifact)).unwrap();

        let err =
            verify_artifact(&artifact).expect_err("должен быть отказ на подменённом контенте");
        assert!(matches!(err, VerifyError::Verification(_)));
    }

    #[test]
    fn tampered_bundle_signature_bytes_fail() {
        let valid_artifact = fixtures_dir().join("RELEASE.txt");
        let valid_bundle_json =
            fs::read_to_string(fixtures_dir().join("RELEASE.txt.sigstore.json")).unwrap();

        let dir = tempdir();
        let artifact = dir.path().join("RELEASE.txt");
        fs::copy(&valid_artifact, &artifact).unwrap();

        // Портим байты подписи внутри бандла (base64-строка поля
        // `messageSignature.signature`/DSSE-подписи) — не трогая
        // остальную структуру JSON, чтобы дойти до реальной криптопроверки,
        // а не отвалиться раньше на парсинге.
        let corrupted = valid_bundle_json.replacen('A', "B", 1);
        fs::write(bundle_path_for(&artifact), corrupted).unwrap();

        let err = verify_artifact(&artifact).expect_err("должен быть отказ на испорченной подписи");
        assert!(matches!(
            err,
            VerifyError::Verification(_) | VerifyError::ParseBundle { .. }
        ));
    }

    fn tempdir() -> TempDir {
        TempDir::new()
    }

    struct TempDir {
        path: std::path::PathBuf,
    }

    impl TempDir {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "berimor-verify-test-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            fs::create_dir_all(&path).unwrap();
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}
