//! Jail файловой системы: канонизация путей, защита от symlink-обхода.
//!
//! Источник: `docs/arch/security-model.md` §1 («Выход за рабочую область:
//! обход через абсолютные пути, симлинки, цепочки команд»), §2 (слой L3).
//! ROADMAP: S2.
//!
//! Отличие от текстовой проверки путей в `deny.rs` (S1): там цель
//! нормализуется лексически, без обращения к ФС — этого достаточно, чтобы
//! поймать `../` и абсолютные пути в тексте команды, но НЕ симлинк,
//! указывающий наружу: лексически `root/link/file` внутри области,
//! физически — где угодно. Здесь путь канонизируется через саму ФС
//! (`std::fs::canonicalize` разрешает все симлинки существующей части), и
//! только после этого сравнивается с каноническим корнем.
//!
//! Известные ограничения (честно, для ревью):
//! - между проверкой и реальным обращением к ФС существует окно TOCTOU —
//!   злоумышленник с параллельным доступом к ФС может подменить симлинк
//!   после проверки. Для модели угроз системы (один писатель на инстанс,
//!   агент — единственный автор действий в области) это окно не входит в
//!   перечень угроз §1; закрытие через `openat2(RESOLVE_BENEATH)` —
//!   платформо-специфичная задача за пределами этого milestone;
//! - запись через ЖЁСТКУЮ ссылку внутри области на внешний inode jail не
//!   видит (находка m13 XL-ревью): hardlink не отличим от обычного файла
//!   без проверки inode. Создание hardlink'ов наружу закрывается тем, что
//!   `ln` на внешнюю цель — мутация вне области для deny-статики лишь
//!   тогда, когда цель текстуально вне; полное закрытие — та же
//!   платформо-специфичная задача, что и TOCTOU.

use std::path::{Component, Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum JailError {
    #[error("корень рабочей области недоступен: {path}: {reason}")]
    RootUnavailable { path: PathBuf, reason: String },
    #[error("путь выходит за рабочую область: {0}")]
    EscapesJail(PathBuf),
    #[error("не удалось канонизировать путь: {path}: {reason}")]
    Canonicalize { path: PathBuf, reason: String },
}

/// Изолированная рабочая область. Создаётся один раз на процесс; корень
/// канонизируется при создании — все сравнения идут с физическим путём.
pub struct FsJail {
    root: PathBuf,
}

impl FsJail {
    /// Корень обязан существовать и быть каталогом — jail над
    /// несуществующим корнем не имеет смысла (канонизировать нечего).
    /// Корень ФС отклоняется: jail, внутри которого лежит всё, ничего не
    /// изолирует, и его молчаливое создание — почти наверняка ошибка
    /// конфигурации (находка m11 XL-ревью). Проверка — `parent().is_none()`,
    /// а не сравнение с `/` буквально: на Windows `canonicalize("/")`
    /// возвращает путь текущего диска (`\\?\C:\`), не `/`, но у него так же
    /// нет родителя — тот же признак корня, что и на Unix.
    pub fn new(root: &Path) -> Result<Self, JailError> {
        let canonical = std::fs::canonicalize(root).map_err(|err| JailError::RootUnavailable {
            path: root.to_path_buf(),
            reason: err.to_string(),
        })?;
        if !canonical.is_dir() {
            return Err(JailError::RootUnavailable {
                path: root.to_path_buf(),
                reason: "не каталог".into(),
            });
        }
        if canonical.parent().is_none() {
            return Err(JailError::RootUnavailable {
                path: root.to_path_buf(),
                reason: "корень jail не может быть корнем файловой системы".into(),
            });
        }
        Ok(Self { root: canonical })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Разрешает пользовательский путь в физический путь внутри области.
    /// Относительные пути отсчитываются от корня. Симлинки разрешаются
    /// ФС — путь, уходящий через симлинк наружу, отклоняется независимо
    /// от того, как он выглядел текстуально.
    ///
    /// Несуществующий хвост (цель записи, которую ещё предстоит создать)
    /// не ошибка: создать файл внутри области законно.
    ///
    /// Алгоритм — покомпонентный обход (находка C1 независимого ревью S2
    /// + красный CI-windows после первого фикса):
    /// - каждый встреченный симлинк НЕМЕДЛЕННО канонизируется с проверкой
    ///   containment: после этой точки мы идём по физическому пути, и
    ///   ускользнуть позже уже нельзя;
    /// - `..` после СИМЛИНКА — консервативный `EscapesJail`: семантика
    ///   различается по платформам (POSIX: `..` применяется к ЦЕЛИ
    ///   симлинка — `outlink/../x` это `<вне области>/x`; Windows:
    ///   `outlink\..\x` схлопывается лексически в `root\x`). Именно эта
    ///   разница и эксплуатировалась C1 на Linux, а на Windows ломала
    ///   противоположный консервативный кейс. Статически безопасного
    ///   ответа для обеих платформ нет — отказ;
    /// - `..` после реального каталога или несуществующего компонента —
    ///   лексический подъём, одинаковый на обеих платформах (второй
    ///   случай при обращении просто даст ENOENT — выхода наружу нет).
    pub fn resolve(&self, path: &Path) -> Result<PathBuf, JailError> {
        let candidate = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.root.join(path)
        };

        let mut current = if path.is_absolute() {
            PathBuf::new()
        } else {
            self.root.clone()
        };
        for component in candidate.components() {
            match component {
                Component::Prefix(prefix) => current.push(prefix.as_os_str()),
                Component::RootDir => current.push(component.as_os_str()),
                Component::CurDir => {}
                Component::ParentDir => {
                    // См. шапку: `..` после симлинка платформо-зависим.
                    if current.is_symlink() {
                        return Err(JailError::EscapesJail(candidate));
                    }
                    // Выше корня ФС или выше корня jail — вне области.
                    if !current.pop() || !current.starts_with(&self.root) {
                        return Err(JailError::EscapesJail(candidate));
                    }
                }
                Component::Normal(name) => current.push(name),
            }
            // Каждый симлинк — точка принуждения к физическому пути.
            if current.is_symlink() {
                let canonical =
                    std::fs::canonicalize(&current).map_err(|err| JailError::Canonicalize {
                        path: current.clone(),
                        reason: err.to_string(),
                    })?;
                if !canonical.starts_with(&self.root) {
                    return Err(JailError::EscapesJail(candidate));
                }
                current = canonical;
            }
        }
        if !current.starts_with(&self.root) {
            return Err(JailError::EscapesJail(candidate));
        }
        Ok(current)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Свежая область в tempdir: root/sub/file, root/outlink -> внешний
    /// каталог, root/sublink -> sub. Возвращает (jail, КАНОНИЧЕСКИЙ root,
    /// внешний путь). Каталог уникален на тест — параллельный прогон не
    /// должен сталкиваться с чужим teardown.
    ///
    /// Root возвращается канонизированным: на macOS tempdir — симлинк
    /// (`/var` → `/private/var`), и сравнение с неканоническим путём даёт
    /// ложное падение там, где на Linux всё совпадает (поймано в CI).
    fn setup() -> (FsJail, PathBuf, PathBuf) {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let unique = format!(
            "berimor-jail-test-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        );
        let base = std::env::temp_dir().join(unique);
        let _ = fs::remove_dir_all(&base);
        let root = base.join("root");
        let outside = base.join("outside");
        fs::create_dir_all(root.join("sub")).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(root.join("sub/file.txt"), "data").unwrap();
        fs::write(outside.join("secret.txt"), "secret").unwrap();
        symlink_dir(&outside, root.join("outlink"));
        symlink_dir(root.join("sub"), root.join("sublink"));

        let jail = FsJail::new(&root).unwrap();
        let canonical_root = jail.root().to_path_buf();
        (jail, canonical_root, outside)
    }

    #[cfg(unix)]
    fn symlink_dir(target: impl AsRef<Path>, link: impl AsRef<Path>) {
        std::os::unix::fs::symlink(target, link).unwrap();
    }

    #[cfg(windows)]
    fn symlink_dir(target: impl AsRef<Path>, link: impl AsRef<Path>) {
        std::os::windows::fs::symlink_dir(target, link).unwrap();
    }

    fn teardown(root: &Path) {
        let _ = fs::remove_dir_all(root.parent().unwrap());
    }

    #[test]
    fn relative_path_inside_resolves() {
        let (jail, root, _) = setup();
        let resolved = jail.resolve(Path::new("sub/file.txt")).unwrap();
        assert_eq!(resolved, root.join("sub/file.txt"));
        teardown(&root);
    }

    #[test]
    fn absolute_path_inside_resolves() {
        let (jail, root, _) = setup();
        let resolved = jail.resolve(&root.join("sub/file.txt")).unwrap();
        assert_eq!(resolved, root.join("sub/file.txt"));
        teardown(&root);
    }

    #[test]
    fn parent_escape_is_rejected() {
        let (jail, root, _) = setup();
        let result = jail.resolve(Path::new("../outside/secret.txt"));
        assert!(matches!(result, Err(JailError::EscapesJail(_))));
        teardown(&root);
    }

    #[test]
    fn absolute_path_outside_is_rejected() {
        let (jail, root, outside) = setup();
        let result = jail.resolve(&outside.join("secret.txt"));
        assert!(matches!(result, Err(JailError::EscapesJail(_))));
        teardown(&root);
    }

    /// Ключевое отличие от лексической проверки S1: `outlink/secret.txt`
    /// текстуально внутри области, физически — нет.
    #[test]
    fn symlink_escape_is_rejected() {
        let (jail, root, _) = setup();
        let result = jail.resolve(Path::new("outlink/secret.txt"));
        assert!(matches!(result, Err(JailError::EscapesJail(_))));
        teardown(&root);
    }

    #[test]
    fn symlink_pointing_inside_is_accepted() {
        let (jail, root, _) = setup();
        let resolved = jail.resolve(Path::new("sublink/file.txt")).unwrap();
        assert_eq!(resolved, root.join("sub/file.txt"));
        teardown(&root);
    }

    #[test]
    fn nonexistent_leaf_under_existing_dir_is_a_legal_write_target() {
        let (jail, root, _) = setup();
        let resolved = jail.resolve(Path::new("sub/new-file.txt")).unwrap();
        assert_eq!(resolved, root.join("sub/new-file.txt"));
        teardown(&root);
    }

    #[test]
    fn nonexistent_leaf_under_escaping_symlink_is_rejected() {
        let (jail, root, _) = setup();
        let result = jail.resolve(Path::new("outlink/new-file.txt"));
        assert!(matches!(result, Err(JailError::EscapesJail(_))));
        teardown(&root);
    }

    #[test]
    fn jail_root_itself_resolves() {
        let (jail, root, _) = setup();
        let resolved = jail.resolve(Path::new(".")).unwrap();
        assert_eq!(resolved, root);
        teardown(&root);
    }

    /// Регрессионный тест C1 независимого ревью S2: `симлинк + ..` —
    /// ядро резолвит симлинк раньше следующего компонента, лексическая
    /// нормализация делала этот путь «внутренним».
    #[test]
    fn symlink_then_parent_escape_is_rejected() {
        let (jail, root, _) = setup();
        let result = jail.resolve(Path::new("outlink/../outside/secret.txt"));
        assert!(
            matches!(result, Err(JailError::EscapesJail(_))),
            "outlink/../outside/secret.txt обязан отклоняться: {result:?}"
        );
        teardown(&root);
    }

    /// Контроль без ложного срабатывания: `..` после симлинка,
    /// указывающего ВНУТРЬ области, физически остаётся внутри.
    #[test]
    fn parent_after_inside_symlink_is_accepted() {
        let (jail, root, _) = setup();
        let resolved = jail.resolve(Path::new("sublink/../sub/file.txt")).unwrap();
        assert_eq!(resolved, root.join("sub/file.txt"));
        teardown(&root);
    }

    /// `..` после несуществующего компонента — статически разрешимо на
    /// обеих платформах: лексический подъём не ведёт наружу (при реальном
    /// обращении POSIX даст ENOENT на несуществующем компоненте, Windows
    /// схлопнет лексически — в обоих случаях внутри области). Консервативный
    /// отказ первого фикса оказался избыточен И платформо-несовместим
    /// (красный CI-windows) — уточнено покомпонентным алгоритмом.
    #[test]
    fn parent_after_nonexistent_component_resolves_statically() {
        let (jail, root, _) = setup();
        let resolved = jail
            .resolve(Path::new("nonexistent/../sub/file.txt"))
            .unwrap();
        assert_eq!(resolved, root.join("sub/file.txt"));
        teardown(&root);
    }

    /// `..` после существующего реального каталога внутри — легально.
    #[test]
    fn parent_after_existing_dir_inside_is_accepted() {
        let (jail, root, _) = setup();
        let resolved = jail.resolve(Path::new("sub/../sub/file.txt")).unwrap();
        assert_eq!(resolved, root.join("sub/file.txt"));
        teardown(&root);
    }

    #[test]
    fn missing_root_is_an_error_not_a_silent_jail() {
        let result = FsJail::new(Path::new("/nonexistent/berimor-jail-root"));
        assert!(matches!(result, Err(JailError::RootUnavailable { .. })));
    }

    #[test]
    fn filesystem_root_is_rejected_as_jail_root() {
        // Находка m11 XL-ревью: jail с корнем `/` ничего не изолирует.
        let result = FsJail::new(Path::new("/"));
        assert!(matches!(result, Err(JailError::RootUnavailable { .. })));
    }
}
