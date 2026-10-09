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

use super::{Coordinates, Problem};

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

    /// The local Maven repository (`LocalM2RepositoryFinder`): the `<localRepository>` of
    /// `~/.m2/settings.xml`, else of `$M2_HOME/conf/settings.xml`, else `~/.m2/repository`.
    pub fn local_repository() -> Option<PathBuf> {
        let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
            .filter(|home| !home.is_empty())
            .map(PathBuf::from)?;
        let m2_home = std::env::var_os("M2_HOME")
            .filter(|path| !path.is_empty())
            .map(|path| PathBuf::from(path).join("conf").join("settings.xml"));
        let configured = [Some(home.join(".m2").join("settings.xml")), m2_home]
            .into_iter()
            .flatten()
            .find_map(|settings| local_repository_setting(&settings));
        Some(configured.unwrap_or_else(|| home.join(".m2").join("repository")))
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
    // The group is a directory per `.`-separated segment.
    let group_is_safe = coordinates.group.split('.').all(is_safe);
    let mut parts = vec![
        ("group", coordinates.group.as_str(), group_is_safe),
        (
            "artifact",
            &coordinates.artifact,
            is_safe(&coordinates.artifact),
        ),
        ("version", version, is_safe(version)),
    ];
    if let Some(classifier) = &coordinates.classifier {
        parts.push(("classifier", classifier, is_safe(classifier)));
    }
    parts.push(("extension", extension, is_safe(extension)));
    if let Some((part, value, _)) = parts.iter().find(|(_, _, safe)| !safe) {
        return Err(Problem::general(format!(
            "{} names no file in a Maven repository: its {part} `{value}` is not a file name",
            coordinates.pretty(None)
        )));
    }
    let mut path: PathBuf = coordinates.group.split('.').collect();
    path.push(&coordinates.artifact);
    path.push(version);
    path.push(format!("{}.{extension}", coordinates.file_stem()));
    Ok(path)
}

/// Whether `part` of coordinates can be one name in a path, and only that.
fn is_safe(part: &str) -> bool {
    !part.is_empty() && part != "." && part != ".." && !part.contains(['/', '\\', ':', '\0'])
}

/// The `<localRepository>` a Maven `settings.xml` sets, if it exists and sets one.
fn local_repository_setting(settings: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(settings).ok()?;
    let document = roxmltree::Document::parse(&text).ok()?;
    let value = document
        .root_element()
        .children()
        .find(|node| node.tag_name().name() == "localRepository")?
        .text()?
        .trim();
    (!value.is_empty()).then(|| PathBuf::from(value))
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
