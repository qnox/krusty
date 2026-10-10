//! The one way this crate looks at a project's files: bounded, sorted, and never through a
//! symbolic link.
//!
//! Every reader in the toolchain (project and module discovery, build files, source and resource
//! collection, input hashing) goes through [`kind`], [`read_file`], [`walk`] and
//! [`resolve_inside`], so containment cannot be lost between one look and the next. A symbolic link
//! is never followed: met where a project's file or directory should be, it is refused with its
//! path, because following it could read files outside the project and ignoring it could silently
//! build something other than what the toolchain builds. A file is opened without following a
//! link in its last component and read from that handle, so it cannot be swapped for a link
//! between being looked at and being read. Directories above the project root are not the
//! project's and may be links.

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
    TooLarge { path: PathBuf, limit: usize },
    NotAFile(PathBuf),
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
            Self::TooLarge { path, limit } => write!(
                f,
                "{}: krusty-toolchain reads build files of at most {limit} bytes",
                path.display()
            ),
            Self::NotAFile(path) => write!(f, "{}: not a file", path.display()),
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

/// What is at `path`, looked at without following a symbolic link: `None` when nothing is.
pub fn kind(path: &Path) -> Result<Option<Kind>, InventoryError> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            Err(InventoryError::SymbolicLink(path.to_path_buf()))
        }
        Ok(metadata) if metadata.is_dir() => Ok(Some(Kind::Directory)),
        Ok(_) => Ok(Some(Kind::File)),
        // Below a file nothing exists either.
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
            ) =>
        {
            Ok(None)
        }
        Err(error) => Err(InventoryError::Io {
            path: path.to_path_buf(),
            error,
        }),
    }
}

/// The bytes of the file at `path`, at most `limit` of them. The file is opened without following
/// a symbolic link and read from the handle opened.
pub fn read_file(path: &Path, limit: usize) -> Result<Vec<u8>, InventoryError> {
    use std::io::Read;
    let file = open_without_following(path)?;
    let metadata = file.metadata().map_err(io_error(path))?;
    if metadata.file_type().is_symlink() {
        return Err(InventoryError::SymbolicLink(path.to_path_buf()));
    }
    if !metadata.is_file() {
        return Err(InventoryError::NotAFile(path.to_path_buf()));
    }
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(io_error(path))?;
    if bytes.len() > limit {
        return Err(InventoryError::TooLarge {
            path: path.to_path_buf(),
            limit,
        });
    }
    Ok(bytes)
}

#[cfg(unix)]
fn open_without_following(path: &Path) -> Result<std::fs::File, InventoryError> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|error| {
            // `O_NOFOLLOW` fails with `ELOOP` on a link.
            if error.raw_os_error() == Some(libc::ELOOP) {
                InventoryError::SymbolicLink(path.to_path_buf())
            } else {
                InventoryError::Io {
                    path: path.to_path_buf(),
                    error,
                }
            }
        })
}

#[cfg(windows)]
fn open_without_following(path: &Path) -> Result<std::fs::File, InventoryError> {
    use std::os::windows::fs::OpenOptionsExt;
    /// Opens a link itself rather than its target; the caller refuses it by its metadata.
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .map_err(io_error(path))
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
    let mut walk = Walk {
        root,
        limits,
        refuse_deeper,
        found: Vec::new(),
    };
    walk.directory(root, "", 0)?;
    let mut found = walk.found;
    found.sort();
    Ok(found)
}

/// One walk in progress. Each directory's entries are taken in name order, so which entry an error
/// names does not depend on the order the file system lists them in.
struct Walk<'a> {
    root: &'a Path,
    limits: WalkLimits,
    refuse_deeper: bool,
    found: Vec<Entry>,
}

impl Walk<'_> {
    fn directory(
        &mut self,
        directory: &Path,
        relative: &str,
        level: usize,
    ) -> Result<(), InventoryError> {
        let mut items = std::fs::read_dir(directory)
            .map_err(io_error(directory))?
            .map(|item| item.map_err(io_error(directory)))
            .collect::<Result<Vec<_>, _>>()?;
        items.sort_by_key(|item| item.file_name());
        for item in items {
            let path = item.path();
            if level >= self.limits.depth {
                if self.refuse_deeper {
                    return Err(InventoryError::TooDeep {
                        path,
                        limit: self.limits.depth,
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
            if self.found.len() >= self.limits.entries {
                return Err(InventoryError::TooMany {
                    root: self.root.to_path_buf(),
                    limit: self.limits.entries,
                });
            }
            if file_type.is_dir() {
                self.found.push(Entry {
                    relative: child.clone(),
                    kind: Kind::Directory,
                });
                self.directory(&path, &child, level + 1)?;
            } else {
                self.found.push(Entry {
                    relative: child,
                    kind: Kind::File,
                });
            }
        }
        Ok(())
    }
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
                if kind(&path)? != Some(Kind::Directory) {
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

    #[cfg(unix)]
    #[test]
    fn a_file_is_read_only_when_it_is_not_a_symbolic_link() {
        let temp = TempDir::new("read");
        write(&temp.0, "real.yaml");
        std::fs::write(temp.0.join("real.yaml"), "abc").unwrap();
        assert_eq!(read_file(&temp.0.join("real.yaml"), 3).unwrap(), b"abc");
        assert_eq!(
            read_file(&temp.0.join("real.yaml"), 2)
                .unwrap_err()
                .to_string(),
            format!(
                "{}: krusty-toolchain reads build files of at most 2 bytes",
                temp.0.join("real.yaml").display()
            )
        );
        std::os::unix::fs::symlink(temp.0.join("real.yaml"), temp.0.join("linked.yaml")).unwrap();
        assert_eq!(
            read_file(&temp.0.join("linked.yaml"), 3)
                .unwrap_err()
                .to_string(),
            format!(
                "krusty-toolchain does not follow symbolic links: {}",
                temp.0.join("linked.yaml").display()
            )
        );
        assert_eq!(
            kind(&temp.0.join("linked.yaml")).unwrap_err().to_string(),
            format!(
                "krusty-toolchain does not follow symbolic links: {}",
                temp.0.join("linked.yaml").display()
            )
        );
        assert_eq!(
            read_file(&temp.0, 3).unwrap_err().to_string(),
            format!("{}: not a file", temp.0.display())
        );
    }

    /// Several entries of one directory would each stop the walk; the first in name order is the
    /// one reported, whatever order the file system lists them in.
    #[cfg(unix)]
    #[test]
    fn of_competing_failures_the_first_entry_by_name_is_reported() {
        use std::os::unix::ffi::OsStrExt;
        let temp = TempDir::new("competing");
        let outside = TempDir::new("competing-target");
        for name in ["d", "b", "f"] {
            std::os::unix::fs::symlink(&outside.0, temp.0.join(name)).unwrap();
        }
        let not_unicode = temp.0.join(std::ffi::OsStr::from_bytes(b"c\xff"));
        std::fs::write(&not_unicode, "").unwrap();
        write(&temp.0, "a/x.kt");
        write(&temp.0, "e.kt");
        let error = walk(&temp.0, WalkLimits::SOURCES, true).unwrap_err();
        assert_eq!(
            error.to_string(),
            format!(
                "krusty-toolchain does not follow symbolic links: {}",
                temp.0.join("b").display()
            )
        );
        std::fs::remove_file(temp.0.join("b")).unwrap();
        let error = walk(&temp.0, WalkLimits::SOURCES, true).unwrap_err();
        assert_eq!(
            error.to_string(),
            format!(
                "krusty-toolchain reads only file names that are valid Unicode: {}",
                not_unicode.display()
            )
        );
        // Of several directories too deep, the first by name is reported.
        std::fs::remove_file(&not_unicode).unwrap();
        for name in ["d", "f"] {
            std::fs::remove_file(temp.0.join(name)).unwrap();
        }
        write(&temp.0, "g/y.kt");
        let shallow = WalkLimits {
            depth: 1,
            entries: 100,
        };
        assert_eq!(
            walk(&temp.0, shallow, true).unwrap_err().to_string(),
            format!(
                "{} is nested more than 1 directories deep; krusty-toolchain stops there",
                temp.0.join("a/x.kt").display()
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
