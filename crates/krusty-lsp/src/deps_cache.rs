//! Content-addressed cache for browsable dependency sources.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use fs2::FileExt;

use crate::project::fingerprint::Hasher;

pub const DEPS_STUB_FORMAT_VERSION: u32 = 1;

pub fn default_cache_root(env: &dyn Fn(&str) -> Option<String>) -> PathBuf {
    let nonempty = |key: &str| env(key).filter(|value| !value.is_empty());
    if let Some(xdg) = nonempty("XDG_CACHE_HOME") {
        return PathBuf::from(xdg).join("krusty").join("deps");
    }
    #[cfg(windows)]
    if let Some(local) = nonempty("LOCALAPPDATA") {
        return PathBuf::from(local).join("krusty").join("deps");
    }
    if let Some(home) = nonempty("HOME") {
        return PathBuf::from(home)
            .join(".cache")
            .join("krusty")
            .join("deps");
    }
    PathBuf::from(".krusty").join("deps")
}

fn content_dir(root: &Path, text: &str) -> PathBuf {
    let mut hasher = Hasher::default();
    hasher.write_str("dependency-source");
    hasher.write(text.as_bytes());
    let key = format!("{:016x}", hasher.finish().as_u64());
    managed_entry_dir(root, &key)
}

/// One independently evictable entry under the shared dependency-cache lifecycle.
///
/// Dependency sources and auxiliary indexes use distinct names in this directory but share the
/// same version root, access timestamp, global lock, age policy, byte ceiling, and `cache clean`
/// behavior. Adding a second top-level cache tree would silently escape all of those bounds.
pub(crate) fn managed_entry_dir(root: &Path, key: &str) -> PathBuf {
    root.join(format!("v{DEPS_STUB_FORMAT_VERSION}")).join(key)
}

pub(crate) fn touch_entry(directory: &Path) -> io::Result<()> {
    fs::create_dir_all(directory)?;
    fs::write(directory.join(".hit"), [])
}

fn encoded_internal_path(internal: &str) -> PathBuf {
    let mut path = PathBuf::new();
    for segment in internal.split('/') {
        let mut encoded = String::new();
        for byte in segment.bytes() {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'$') {
                encoded.push(char::from(byte));
            } else {
                encoded.push_str(&format!("_{byte:02x}"));
            }
        }
        path.push(if encoded.is_empty() { "_00" } else { &encoded });
    }
    path.set_extension("kt");
    path
}

pub fn cache_path(root: &Path, internal: &str, text: &str) -> PathBuf {
    content_dir(root, text).join(encoded_internal_path(internal))
}

fn global_lock_path(root: &Path) -> PathBuf {
    let name = root
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("deps");
    root.parent()
        .unwrap_or_else(|| Path::new("."))
        .join(format!(".{name}.lock"))
}

pub(crate) fn global_lock(root: &Path) -> io::Result<fs::File> {
    if let Some(parent) = root.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::File::create(global_lock_path(root))
}

pub fn store(root: &Path, internal: &str, text: &str) -> io::Result<PathBuf> {
    let global = global_lock(root)?;
    FileExt::lock_shared(&global)?;
    let path = cache_path(root, internal, text);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let _ = touch_entry(&content_dir(root, text));
    let lock_path = path.with_extension("lock");
    let lock = fs::File::create(&lock_path)?;
    lock.lock_exclusive()?;
    if fs::read_to_string(&path).map_or(true, |cached| cached != text) {
        let tmp = path.with_extension("kt.tmp");
        fs::write(&tmp, text)?;
        fs::rename(&tmp, &path)?;
    }
    let _ = FileExt::unlock(&lock);
    Ok(path)
}

#[derive(Default, Debug)]
pub struct GcStats {
    pub evicted: usize,
    pub bytes_freed: u64,
}

pub fn gc(root: &Path, max_age_days: u64, max_bytes: u64, now_secs: u64) -> io::Result<GcStats> {
    let mut stats = GcStats::default();
    if !root.is_dir() {
        return Ok(stats);
    }
    let lock = global_lock(root)?;
    if lock.try_lock_exclusive().is_err() {
        return Ok(stats);
    }
    let version_dir = root.join(format!("v{DEPS_STUB_FORMAT_VERSION}"));
    for entry in fs::read_dir(root)? {
        let path = entry?.path();
        let obsolete = path.is_dir()
            && path != version_dir
            && path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with('v'));
        if obsolete {
            evict(&path, directory_size(&path), &mut stats);
        }
    }
    if !version_dir.is_dir() {
        return Ok(stats);
    }

    let mut jar_dirs = Vec::new();
    for entry in fs::read_dir(&version_dir)? {
        let path = entry?.path();
        if !path.is_dir() {
            continue;
        }
        let last_hit = path
            .join(".hit")
            .metadata()
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |since| since.as_secs());
        let size = directory_size(&path);
        jar_dirs.push((path, last_hit, size));
    }

    let max_age_secs = max_age_days.saturating_mul(86_400);
    jar_dirs.retain(|(path, last_hit, size)| {
        if now_secs.saturating_sub(*last_hit) > max_age_secs {
            evict(path, *size, &mut stats);
            false
        } else {
            true
        }
    });

    let mut total: u64 = jar_dirs.iter().map(|(_, _, size)| *size).sum();
    if total > max_bytes {
        jar_dirs.sort_by_key(|(_, last_hit, _)| *last_hit);
        let watermark = max_bytes / 10 * 8;
        for (path, _, size) in &jar_dirs {
            if total <= watermark {
                break;
            }
            evict(path, *size, &mut stats);
            total = total.saturating_sub(*size);
        }
    }

    Ok(stats)
}

pub fn clean(root: &Path, all: bool) -> io::Result<u64> {
    let lock = global_lock(root)?;
    lock.lock_exclusive()?;
    let target = if all {
        root.to_path_buf()
    } else {
        root.join(format!("v{DEPS_STUB_FORMAT_VERSION}"))
    };
    if !target.exists() {
        return Ok(0);
    }
    let freed = directory_size(&target);
    fs::remove_dir_all(&target)?;
    Ok(freed)
}

fn evict(path: &Path, size: u64, stats: &mut GcStats) {
    if fs::remove_dir_all(path).is_ok() {
        stats.evicted += 1;
        stats.bytes_freed += size;
    }
}

fn directory_size(dir: &Path) -> u64 {
    let mut total = 0;
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            total += directory_size(&path);
        } else if let Ok(meta) = path.metadata() {
            total += meta.len();
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::SystemTime;

    #[test]
    fn cache_root_prefers_xdg_then_home() {
        let xdg = |key: &str| (key == "XDG_CACHE_HOME").then(|| "/x/cache".to_string());
        assert_eq!(
            default_cache_root(&xdg),
            PathBuf::from("/x/cache/krusty/deps")
        );

        let home = |key: &str| (key == "HOME").then(|| "/u/me".to_string());
        assert_eq!(
            default_cache_root(&home),
            PathBuf::from("/u/me/.cache/krusty/deps")
        );
    }

    #[test]
    fn cache_path_is_versioned_content_addressed_and_safe() {
        let path = cache_path(
            Path::new("/c"),
            "../kotlin\\collections/CollectionsKt",
            "class CollectionsKt",
        );
        assert!(path.starts_with(format!("/c/v{DEPS_STUB_FORMAT_VERSION}/")));
        assert!(path.ends_with("_2e_2e/kotlin_5ccollections/CollectionsKt.kt"));
        assert!(!path.to_string_lossy().contains("/../"));
    }

    #[test]
    fn clean_removes_the_format_tree_or_the_whole_root() {
        let dir = std::env::temp_dir().join(format!("krusty-clean-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);

        store(&dir, "pkg/A", "aaaa").unwrap();
        let freed = clean(&dir, false).unwrap();
        assert!(freed > 0, "no bytes reported freed");
        assert!(!dir.join(format!("v{DEPS_STUB_FORMAT_VERSION}")).exists());
        assert!(
            dir.exists(),
            "default clean should not remove the root itself"
        );

        store(&dir, "pkg/A", "aaaa").unwrap();
        clean(&dir, true).unwrap();
        assert!(!dir.exists(), "clean --all should remove the whole root");
    }

    #[test]
    fn gc_evicts_stale_entries_and_keeps_fresh_ones() {
        let dir = std::env::temp_dir().join(format!("krusty-gc-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let path = store(&dir, "pkg/A", "aaaa").unwrap();
        let content_dir = path.parent().unwrap().parent().unwrap().to_path_buf();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let stats = gc(&dir, 30, u64::MAX, now + 100 * 86_400).unwrap();
        assert_eq!(stats.evicted, 1);
        assert!(!content_dir.exists(), "stale entry not evicted");

        let path = store(&dir, "pkg/A", "aaaa").unwrap();
        let stats = gc(&dir, 30, u64::MAX, now).unwrap();
        assert_eq!(stats.evicted, 0);
        assert!(path.exists(), "fresh entry wrongly evicted");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn gc_removes_obsolete_formats_and_enforces_the_size_limit() {
        let dir = std::env::temp_dir().join(format!("krusty-gc-size-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("v0/old")).unwrap();
        fs::write(dir.join("v0/old/Foo.kt"), "old").unwrap();
        store(&dir, "pkg/A", &"a".repeat(128)).unwrap();
        store(&dir, "pkg/B", &"b".repeat(128)).unwrap();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let stats = gc(&dir, 30, 1, now).unwrap();

        assert!(!dir.join("v0").exists());
        assert!(stats.evicted >= 3);
        assert_eq!(
            directory_size(&dir.join(format!("v{DEPS_STUB_FORMAT_VERSION}"))),
            0
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn store_writes_atomically_and_touches_the_hit_marker() {
        let dir = std::env::temp_dir().join(format!("krusty-cache-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let path = store(&dir, "pkg/Foo", "package pkg\nclass Foo").unwrap();

        assert_eq!(fs::read_to_string(&path).unwrap(), "package pkg\nclass Foo");
        assert!(path
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join(".hit")
            .exists());
        let again = store(&dir, "pkg/Foo", "package pkg\nclass Foo").unwrap();
        assert_eq!(again, path);
        let changed = store(&dir, "pkg/Foo", "package pkg\nclass Foo(val x: Int)").unwrap();
        assert_ne!(changed, path);
        assert_eq!(
            fs::read_to_string(changed).unwrap(),
            "package pkg\nclass Foo(val x: Int)"
        );

        let _ = fs::remove_dir_all(&dir);
    }
}
