//! Where resolution reads POMs and Gradle module metadata: the toolchain's shared artifact cache
//! (`<cache root>/.m2.cache`) and, for projects that list `mavenLocal`, the local Maven repository
//! before it. Both are laid out as Maven repositories.
//!
//! Coordinates come from build files and from other artifacts' metadata, so every part is checked
//! before it names a path: a part that is empty, `.`, `..`, or holds a path separator, a drive or
//! stream separator (`:`) or NUL names no file, and reading it is a problem rather than a path
//! outside the repository.

use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use super::{Coordinates, ParseError, Problem};

pub struct Store {
    /// The repositories read, in order; the cache is the last.
    repositories: Vec<PathBuf>,
}

/// A metadata file read from a repository.
#[derive(Debug)]
pub struct StoredFile {
    pub path: PathBuf,
    pub text: Rc<str>,
}

impl Store {
    /// The store under the cache root `root`.
    pub fn new(root: &Path) -> Self {
        Self {
            repositories: vec![root.join(".m2.cache")],
        }
    }

    /// The store under the cache root `root`, reading the local Maven repository `local` first.
    pub fn with_local(local: &Path, root: &Path) -> Self {
        Self {
            repositories: vec![local.to_path_buf(), root.join(".m2.cache")],
        }
    }

    /// The local Maven repository (`LocalM2RepositoryFinder`) of this host: see
    /// [`local_repository_in`]. `None` when there is no home directory to find it in.
    pub fn local_repository() -> Result<Option<PathBuf>, Problem> {
        let variable = |name: &str| {
            std::env::var_os(name)
                .filter(|value| !value.to_string_lossy().trim().is_empty())
                .map(PathBuf::from)
        };
        let Some(home) = variable(if cfg!(windows) { "USERPROFILE" } else { "HOME" }) else {
            return Ok(None);
        };
        local_repository_in(&home, variable("M2_HOME").as_deref()).map(Some)
    }

    /// The cache root the toolchain uses on this host (`AmperUserCacheRoot`): the
    /// `KOTLIN_SHARED_CACHE_DIR` (or deprecated `AMPER_SHARED_CACHE_DIR`) variable, else the
    /// platform's user cache directory.
    pub fn default_root() -> Option<PathBuf> {
        let variable = |name: &str| {
            std::env::var_os(name)
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
        };
        if let Some(root) =
            variable("KOTLIN_SHARED_CACHE_DIR").or_else(|| variable("AMPER_SHARED_CACHE_DIR"))
        {
            return Some(root);
        }
        let caches = if cfg!(windows) {
            variable("LOCALAPPDATA")?
        } else if cfg!(target_os = "macos") {
            variable("HOME")?.join("Library").join("Caches")
        } else {
            match variable("XDG_CACHE_HOME") {
                Some(caches) => caches,
                None => variable("HOME")?.join(".cache"),
            }
        };
        Some(caches.join("JetBrains").join("Kotlin"))
    }

    /// The file of `coordinates` with `extension` from the first repository that holds it, or
    /// `None` when none does. A file that is there but cannot be read, and coordinates that name
    /// no file, are problems.
    pub fn read(
        &self,
        coordinates: &Coordinates,
        extension: &str,
    ) -> Result<Option<StoredFile>, Problem> {
        let relative = relative_path(coordinates, extension)?;
        for repository in &self.repositories {
            let path = repository.join(&relative);
            let bytes = match std::fs::read(&path) {
                Ok(bytes) => bytes,
                Err(error) if is_absent(&error) => continue,
                Err(error) => {
                    return Err(Problem::in_file(
                        &path,
                        None,
                        format!("cannot read {}: {error}", coordinates.pretty(None)),
                    ))
                }
            };
            let text = String::from_utf8(bytes).map_err(|_| {
                Problem::in_file(
                    &path,
                    None,
                    format!("the metadata of {} is not UTF-8", coordinates.pretty(None)),
                )
            })?;
            return Ok(Some(StoredFile {
                path,
                text: Rc::from(text),
            }));
        }
        Ok(None)
    }
}

/// Whether reading a file failed only because it is not there.
fn is_absent(error: &std::io::Error) -> bool {
    matches!(error.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory)
}

/// Where a Maven repository keeps the file of `coordinates` with `extension`, relative to it:
/// `group/as/directories/artifact/version/artifact-version[-classifier].extension`.
fn relative_path(coordinates: &Coordinates, extension: &str) -> Result<PathBuf, Problem> {
    let version = coordinates.version.as_deref().unwrap_or("unspecified");
    if let Some(message) = coordinates.repository_path_problem(extension) {
        return Err(Problem::general(message));
    }
    let mut path: PathBuf = coordinates.group.split('.').collect();
    path.push(&coordinates.artifact);
    path.push(version);
    path.push(format!("{}.{extension}", coordinates.file_stem()));
    Ok(path)
}

/// The local Maven repository for the home directory `home` and Maven installation `m2_home`:
/// the `<localRepository>` of `<home>/.m2/settings.xml`, else of `<m2_home>/conf/settings.xml`,
/// else `<home>/.m2/repository`. Only an absent settings file, or one that sets no
/// `<localRepository>`, falls through to the next; one that cannot be read, is not a settings
/// file, or sets an empty location or one through a property (which is not substituted) is a
/// problem. The toolchain ignores such a file and reads another repository than the one it names.
fn local_repository_in(home: &Path, m2_home: Option<&Path>) -> Result<PathBuf, Problem> {
    let user = home.join(".m2").join("settings.xml");
    let installation = m2_home.map(|m2_home| m2_home.join("conf").join("settings.xml"));
    for settings in std::iter::once(user).chain(installation) {
        if let Some(local) = local_repository_setting(&settings)? {
            return Ok(local);
        }
    }
    Ok(home.join(".m2").join("repository"))
}

/// The `<localRepository>` the Maven settings file `settings` sets; `None` when there is no such
/// file or it sets none.
fn local_repository_setting(settings: &Path) -> Result<Option<PathBuf>, Problem> {
    let bytes = match std::fs::read(settings) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(Problem::in_file(
                settings,
                None,
                format!("cannot read the Maven settings: {error}"),
            ));
        }
    };
    let text = String::from_utf8(bytes)
        .map_err(|_| Problem::in_file(settings, None, "the Maven settings are not UTF-8"))?;
    let at = |document: &roxmltree::Document, node: roxmltree::Node, message: &str| {
        let position = document.text_pos_at(node.range().start);
        let error = ParseError::at(position.row as usize, position.col as usize, message, "");
        Problem::in_file(settings, error.position, error.message)
    };
    let document = roxmltree::Document::parse(&text).map_err(|error| {
        let position = error.pos();
        let error = ParseError::at(
            position.row as usize,
            position.col as usize,
            &format!("the Maven settings are not well-formed XML: {error}"),
            &format!(" at {position}"),
        );
        Problem::in_file(settings, error.position, error.message)
    })?;
    let root = document.root_element();
    if root.tag_name().name() != "settings" {
        return Err(at(
            &document,
            root,
            &format!(
                "the Maven settings' root element is `{}`, not `settings`",
                root.tag_name().name()
            ),
        ));
    }
    let Some(element) = root
        .children()
        .find(|node| node.tag_name().name() == "localRepository")
    else {
        return Ok(None);
    };
    let value = element.text().unwrap_or_default().trim();
    if value.is_empty() {
        return Err(at(
            &document,
            element,
            "`localRepository` is empty: it names no local Maven repository",
        ));
    }
    if value.contains("${") {
        return Err(at(
            &document,
            element,
            &format!(
                "`localRepository` `{value}` uses a property, and krusty-toolchain does not substitute properties in the Maven settings"
            ),
        ));
    }
    Ok(Some(PathBuf::from(value)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::yaml::Position;

    fn temp(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("krusty-store-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn every_part_of_coordinates_must_be_one_file_name() {
        let root = temp("parts");
        // A file each hostile coordinate would reach if it were joined as a path.
        std::fs::write(root.join("escaped.pom"), "<project/>").unwrap();
        let store = Store::new(&root.join("cache"));
        let cases = [
            (
                Coordinates::new("org..example", "a", Some("1")),
                "group `org..example`",
            ),
            (Coordinates::new("..", "a", Some("1")), "group `..`"),
            (
                Coordinates::new("org.example", "../../..", Some("1")),
                "artifact `../../..`",
            ),
            (
                Coordinates::new("org.example", ".", Some("1")),
                "artifact `.`",
            ),
            (
                Coordinates::new("org.example", "a\\b", Some("1")),
                "artifact `a\\b`",
            ),
            (
                Coordinates::new("org.example", "a", Some("..")),
                "version `..`",
            ),
            (
                Coordinates::new("org.example", "a", Some("C:x")),
                "version `C:x`",
            ),
            (
                Coordinates::new("org.example", "a", Some("1\0")),
                "version `1\0`",
            ),
            (
                Coordinates::with_selector("org.example", "a", Some("1"), Some("/x"), None),
                "classifier `/x`",
            ),
        ];
        for (coordinates, part) in cases {
            let problem = store.read(&coordinates, "pom").unwrap_err();
            assert_eq!(
                problem,
                Problem::general(format!(
                    "{} names no file in a Maven repository: its {} is not a file name",
                    coordinates.pretty(None),
                    part
                ))
            );
        }
        let coordinates = Coordinates::new("org.example", "a", Some("1"));
        assert_eq!(
            store.read(&coordinates, "../escaped").unwrap_err(),
            Problem::general(
                "org.example:a:1 names no file in a Maven repository: its extension `../escaped` is not a file name"
            )
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn only_an_absent_file_falls_through_to_the_next_repository() {
        let root = temp("fall-through");
        let local = root.join("local");
        let cache = root.join("cache/.m2.cache");
        let coordinates = Coordinates::new("org.example", "a", Some("1"));
        let in_cache = cache.join("org/example/a/1/a-1.pom");
        std::fs::create_dir_all(in_cache.parent().unwrap()).unwrap();
        std::fs::write(&in_cache, "cached").unwrap();
        let store = Store::with_local(&local, &root.join("cache"));
        let found = store.read(&coordinates, "pom").unwrap().unwrap();
        assert_eq!((found.path, &*found.text), (in_cache.clone(), "cached"));
        assert!(store.read(&coordinates, "module").unwrap().is_none());

        // A local file that is not UTF-8 is a problem, not a reason to read the cache.
        let in_local = local.join("org/example/a/1/a-1.pom");
        std::fs::create_dir_all(in_local.parent().unwrap()).unwrap();
        std::fs::write(&in_local, [0xff, 0xfe]).unwrap();
        assert_eq!(
            store.read(&coordinates, "pom").unwrap_err(),
            Problem::in_file(
                &in_local,
                None,
                "the metadata of org.example:a:1 is not UTF-8"
            )
        );

        // Nor is one that cannot be read at all: here, a directory where the file should be.
        std::fs::remove_file(&in_local).unwrap();
        std::fs::create_dir(&in_local).unwrap();
        let problem = store.read(&coordinates, "pom").unwrap_err();
        assert_eq!(problem.file.as_deref(), Some(in_local.as_path()));
        assert_eq!(problem.position, None);
        assert_eq!(
            problem.message,
            format!(
                "cannot read org.example:a:1: {}",
                std::fs::read(&in_local).unwrap_err()
            )
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_user_settings_win_over_the_installation_settings_over_the_default() {
        let root = temp("local-precedence");
        let home = root.join("home");
        let m2_home = root.join("maven");
        let user = home.join(".m2/settings.xml");
        let installation = m2_home.join("conf/settings.xml");
        std::fs::create_dir_all(user.parent().unwrap()).unwrap();
        std::fs::create_dir_all(installation.parent().unwrap()).unwrap();
        let default = home.join(".m2/repository");

        // No settings at all, and settings that set no location, fall through to the default.
        assert_eq!(
            local_repository_in(&home, Some(&m2_home)),
            Ok(default.clone())
        );
        std::fs::write(&user, "<settings><offline>true</offline></settings>").unwrap();
        assert_eq!(
            local_repository_in(&home, Some(&m2_home)),
            Ok(default.clone())
        );

        std::fs::write(
            &installation,
            "<settings><localRepository>/from/installation</localRepository></settings>",
        )
        .unwrap();
        assert_eq!(
            local_repository_in(&home, Some(&m2_home)),
            Ok(PathBuf::from("/from/installation"))
        );
        assert_eq!(local_repository_in(&home, None), Ok(default));

        std::fs::write(
            &user,
            "<settings>\n  <localRepository> /from/user </localRepository>\n</settings>",
        )
        .unwrap();
        assert_eq!(
            local_repository_in(&home, Some(&m2_home)),
            Ok(PathBuf::from("/from/user"))
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn settings_that_exist_but_name_no_repository_are_problems() {
        let root = temp("local-invalid");
        let home = root.join("home");
        let user = home.join(".m2/settings.xml");
        std::fs::create_dir_all(user.parent().unwrap()).unwrap();
        // The installation's settings would name a repository; an invalid user file stops there.
        let m2_home = root.join("maven");
        std::fs::create_dir_all(m2_home.join("conf")).unwrap();
        std::fs::write(
            m2_home.join("conf/settings.xml"),
            "<settings><localRepository>/from/installation</localRepository></settings>",
        )
        .unwrap();
        let position = |line, column| Some(Position { line, column });
        let cases: [(&[u8], Problem); 5] = [
            (
                b"<settings>\n  <localRepository>\n",
                Problem::in_file(
                    &user,
                    position(1, 1),
                    "the Maven settings are not well-formed XML: the root node was opened but never closed",
                ),
            ),
            (
                b"<project/>",
                Problem::in_file(
                    &user,
                    position(1, 1),
                    "the Maven settings' root element is `project`, not `settings`",
                ),
            ),
            (
                b"<settings>\n  <localRepository>  </localRepository>\n</settings>",
                Problem::in_file(
                    &user,
                    position(2, 3),
                    "`localRepository` is empty: it names no local Maven repository",
                ),
            ),
            (
                b"<settings><localRepository>${user.home}/m2</localRepository></settings>",
                Problem::in_file(
                    &user,
                    position(1, 11),
                    "`localRepository` `${user.home}/m2` uses a property, and krusty-toolchain does not substitute properties in the Maven settings",
                ),
            ),
            (
                &[0xff, 0xfe],
                Problem::in_file(&user, None, "the Maven settings are not UTF-8"),
            ),
        ];
        for (text, problem) in cases {
            std::fs::write(&user, text).unwrap();
            assert_eq!(local_repository_in(&home, Some(&m2_home)), Err(problem));
        }

        // Nor is one that cannot be read at all: here, a directory where the file should be.
        std::fs::remove_file(&user).unwrap();
        std::fs::create_dir(&user).unwrap();
        assert_eq!(
            local_repository_in(&home, Some(&m2_home)),
            Err(Problem::in_file(
                &user,
                None,
                format!(
                    "cannot read the Maven settings: {}",
                    std::fs::read(&user).unwrap_err()
                )
            ))
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
