//! The one way this crate looks at a project's files: bounded, sorted, and never through a
//! symbolic link.
//!
//! Every reader in the toolchain (module discovery, source and resource collection, input hashing)
//! goes through [`walk`] and [`resolve_inside`], so containment cannot be lost between one walk and
//! the next. A symbolic link is never followed: met inside a walked region it is refused with its
//! path, because following it could read files outside the project and ignoring it could silently
//! build something other than what the toolchain builds.

use std::fmt;
use std::io;
use std::path::{Component, Path, PathBuf};

/// Bounds on one walk.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WalkLimits {
    /// Directory levels below the root that are listed (the root's own entries are level 1).
    pub depth: usize,
    /// Entries listed in total, files and directories.
    pub entries: usize,
}

impl WalkLimits {
    /// Bounds for a source or resource root.
    pub const SOURCES: WalkLimits = WalkLimits {
        depth: 64,
        entries: 1_000_000,
    };
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    Directory,
    File,
}

/// One entry below a walk's root.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Entry {
    /// `/`-separated path relative to the walk's root.
    pub relative: String,
    pub kind: Kind,
}

#[derive(Debug)]
pub enum InventoryError {
    SymbolicLink(PathBuf),
    TooDeep { path: PathBuf, limit: usize },
    TooMany { root: PathBuf, limit: usize },
    NotUnicode(PathBuf),
    Io { path: PathBuf, error: io::Error },
}

impl fmt::Display for InventoryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SymbolicLink(path) => write!(
                f,
                "krusty-toolchain does not follow symbolic links: {}",
                path.display()
            ),
            Self::TooDeep { path, limit } => write!(
                f,
                "{} is nested more than {limit} directories deep; krusty-toolchain stops there",
                path.display()
            ),
            Self::TooMany { root, limit } => write!(
                f,
                "{} holds more than {limit} files and directories; krusty-toolchain stops there",
                root.display()
            ),
            Self::NotUnicode(path) => write!(
                f,
                "krusty-toolchain reads only file names that are valid Unicode: {}",
                path.display()
            ),
            Self::Io { path, error } => write!(f, "{}: {error}", path.display()),
        }
    }
}

fn io_error(path: &Path) -> impl FnOnce(io::Error) -> InventoryError + '_ {
    move |error| InventoryError::Io {
        path: path.to_path_buf(),
        error,
    }
}

/// Every file and directory below `root`, at most `limits.depth` levels down, sorted by relative
/// path. `root` itself must be a real directory. Entries below the depth limit are not listed;
/// a directory at the limit with children is an error only when `refuse_deeper` is set, so a walk
/// that needs everything cannot silently stop short.
pub fn walk(
    root: &Path,
    limits: WalkLimits,
    refuse_deeper: bool,
) -> Result<Vec<Entry>, InventoryError> {
    let metadata = std::fs::symlink_metadata(root).map_err(io_error(root))?;
    if metadata.file_type().is_symlink() {
        return Err(InventoryError::SymbolicLink(root.to_path_buf()));
    }
    let mut found = Vec::new();
    // (directory, its relative path, its level)
    let mut pending: Vec<(PathBuf, String, usize)> = vec![(root.to_path_buf(), String::new(), 0)];
    while let Some((directory, relative, level)) = pending.pop() {
        for item in std::fs::read_dir(&directory).map_err(io_error(&directory))? {
            let item = item.map_err(io_error(&directory))?;
            let path = item.path();
            if level >= limits.depth {
                if refuse_deeper {
                    return Err(InventoryError::TooDeep {
                        path,
                        limit: limits.depth,
                    });
                }
                break;
            }
            let name = item
                .file_name()
                .into_string()
                .map_err(|_| InventoryError::NotUnicode(path.clone()))?;
            let child = if relative.is_empty() {
                name
            } else {
                format!("{relative}/{name}")
            };
            let file_type = item.file_type().map_err(io_error(&path))?;
            if file_type.is_symlink() {
                return Err(InventoryError::SymbolicLink(path));
            }
            if found.len() >= limits.entries {
                return Err(InventoryError::TooMany {
                    root: root.to_path_buf(),
                    limit: limits.entries,
                });
            }
            if file_type.is_dir() {
                found.push(Entry {
                    relative: child.clone(),
                    kind: Kind::Directory,
                });
                pending.push((path, child, level + 1));
            } else {
                found.push(Entry {
                    relative: child,
                    kind: Kind::File,
                });
            }
        }
    }
    found.sort();
    Ok(found)
}

/// Where `relative` leads from `root`, walked component by component as the file system would walk
/// it, but without following a symbolic link: `.` is dropped and `..` steps back out of a directory
/// that exists. Below a missing component nothing exists, so the rest is kept as written and the
/// result does not exist either. `Ok(None)` when the path leaves `root`; the caller reports that in
/// its own terms.
pub fn resolve_inside(root: &Path, relative: &Path) -> Result<Option<PathBuf>, InventoryError> {
    let mut path = root.to_path_buf();
    let mut depth = 0usize;
    let mut components = relative.components();
    while let Some(component) = components.next() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if depth == 0 {
                    return Ok(None);
                }
                if !path.is_dir() {
                    return Ok(Some(path.join("..").join(components.as_path())));
                }
                path.pop();
                depth -= 1;
            }
            Component::Normal(segment) => {
                path.push(segment);
                depth += 1;
                match std::fs::symlink_metadata(&path) {
                    Ok(metadata) if metadata.file_type().is_symlink() => {
                        return Err(InventoryError::SymbolicLink(path))
                    }
                    Ok(_) => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {
                        return Ok(Some(path.join(components.as_path())));
                    }
                    Err(error) => return Err(InventoryError::Io { path, error }),
                }
            }
            Component::RootDir | Component::Prefix(_) => return Ok(None),
        }
    }
    Ok(Some(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "krusty-toolchain-inventory-{name}-{}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            Self(path.canonicalize().unwrap())
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn write(root: &Path, relative: &str) {
        let path = root.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "x").unwrap();
    }

    fn listed(entries: &[Entry]) -> Vec<String> {
        entries
            .iter()
            .map(|entry| match entry.kind {
                Kind::Directory => format!("{}/", entry.relative),
                Kind::File => entry.relative.clone(),
            })
            .collect()
    }

    #[test]
    fn a_walk_lists_everything_sorted() {
        let temp = TempDir::new("sorted");
        write(&temp.0, "b/z.kt");
        write(&temp.0, "a.kt");
        write(&temp.0, "b/a/y.kt");
        let entries = walk(&temp.0, WalkLimits::SOURCES, true).unwrap();
        assert_eq!(
            listed(&entries),
            ["a.kt", "b/", "b/a/", "b/a/y.kt", "b/z.kt"]
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_symbolic_link_is_refused_wherever_it_is_met() {
        let temp = TempDir::new("links");
        let outside = TempDir::new("outside");
        write(&outside.0, "Secret.kt");
        write(&temp.0, "src/Real.kt");
        std::os::unix::fs::symlink(&outside.0, temp.0.join("src/escape")).unwrap();
        let error = walk(&temp.0.join("src"), WalkLimits::SOURCES, true).unwrap_err();
        assert_eq!(
            error.to_string(),
            format!(
                "krusty-toolchain does not follow symbolic links: {}",
                temp.0.join("src/escape").display()
            )
        );
        std::os::unix::fs::symlink(outside.0.join("Secret.kt"), temp.0.join("Linked.kt")).unwrap();
        let error = resolve_inside(&temp.0, Path::new("./Linked.kt")).unwrap_err();
        assert_eq!(
            error.to_string(),
            format!(
                "krusty-toolchain does not follow symbolic links: {}",
                temp.0.join("Linked.kt").display()
            )
        );
        let error = resolve_inside(&temp.0, Path::new("src/escape/Secret.kt")).unwrap_err();
        assert_eq!(
            error.to_string(),
            format!(
                "krusty-toolchain does not follow symbolic links: {}",
                temp.0.join("src/escape").display()
            )
        );
    }

    #[test]
    fn walks_stop_at_their_limits_with_the_limit_named() {
        let temp = TempDir::new("limits");
        write(&temp.0, "a/b/c/d.kt");
        let shallow = WalkLimits {
            depth: 2,
            entries: 100,
        };
        let error = walk(&temp.0, shallow, true).unwrap_err();
        assert_eq!(
            error.to_string(),
            format!(
                "{} is nested more than 2 directories deep; krusty-toolchain stops there",
                temp.0.join("a/b/c").display()
            )
        );
        assert_eq!(
            listed(&walk(&temp.0, shallow, false).unwrap()),
            ["a/", "a/b/"]
        );
        let few = WalkLimits {
            depth: 64,
            entries: 2,
        };
        let error = walk(&temp.0, few, true).unwrap_err();
        assert_eq!(
            error.to_string(),
            format!(
                "{} holds more than 2 files and directories; krusty-toolchain stops there",
                temp.0.display()
            )
        );
    }

    #[test]
    fn a_relative_path_resolves_inside_or_reports_leaving() {
        let temp = TempDir::new("resolve");
        write(&temp.0, "libs/a/module.yaml");
        write(&temp.0, "libs/x/readme.txt");
        assert_eq!(
            resolve_inside(&temp.0, Path::new("./libs/x/../a")).unwrap(),
            Some(temp.0.join("libs/a"))
        );
        assert_eq!(
            resolve_inside(&temp.0, Path::new("libs/y/../a")).unwrap(),
            Some(temp.0.join("libs/y/../a"))
        );
        assert_eq!(
            resolve_inside(&temp.0, Path::new("libs/x/readme.txt/../../a")).unwrap(),
            Some(temp.0.join("libs/x/readme.txt/../../a"))
        );
        assert_eq!(
            resolve_inside(&temp.0, Path::new("libs/missing/deeper")).unwrap(),
            Some(temp.0.join("libs/missing/deeper"))
        );
        assert_eq!(
            resolve_inside(&temp.0, Path::new("../elsewhere")).unwrap(),
            None
        );
        assert_eq!(resolve_inside(&temp.0, Path::new("/abs")).unwrap(), None);
    }
}
