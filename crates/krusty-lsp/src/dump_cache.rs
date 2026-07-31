//! Private, bounded on-disk store for dev-mode dumps.
//!
//! A dump contains source identifiers, literals, diagnostics, and lowered instructions. Its cache
//! path is therefore an opaque digest of the document URI, not a copy of the workspace path. The
//! digest also gives every key a fixed-size filename and avoids the aliasing bugs inherent in
//! escaping arbitrary path text. Files remain stable across content changes so an editor buffer
//! left open on a dump can refresh in place.

use std::fmt::Write as _;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::UNIX_EPOCH;

use fs2::FileExt;
use sha2::{Digest, Sha256};

pub const DUMP_FORMAT_VERSION: u32 = 1;

/// A single dump is diagnostic output, not an unbounded export channel. This is deliberately above
/// the worker's 32 MiB source-set limit so ordinary AST/checker/IR expansion has room, while a
/// pathological printer expansion cannot consume the cache indefinitely.
pub(crate) const MAX_DUMP_BYTES: usize = 64 * 1024 * 1024;
/// Cycling through files must not leave one permanent dump per source forever.
const MAX_DUMP_FILES: usize = 64;
const MAX_DUMP_CACHE_BYTES: u64 = 256 * 1024 * 1024;

/// Write `text` as the dump for `key` and return its stable, opaque path.
pub fn store(root: &Path, key: &str, text: &str) -> io::Result<PathBuf> {
    store_with_limits(
        root,
        key,
        text,
        MAX_DUMP_BYTES,
        MAX_DUMP_FILES,
        MAX_DUMP_CACHE_BYTES,
    )
}

fn store_with_limits(
    root: &Path,
    key: &str,
    text: &str,
    max_dump_bytes: usize,
    max_files: usize,
    max_cache_bytes: u64,
) -> io::Result<PathBuf> {
    if text.len() > max_dump_bytes
        || u64::try_from(text.len()).map_or(true, |bytes| bytes > max_cache_bytes)
        || max_files == 0
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "dev dump cannot fit the configured limits ({} bytes, \
                 {max_dump_bytes}-byte file limit, {max_cache_bytes}-byte cache limit, \
                 {max_files}-file limit)",
                text.len(),
            ),
        ));
    }

    let dumps = root.join("dumps");
    ensure_private_directory(&dumps)?;
    let lock_path = dumps.join(".lock");
    if fs::symlink_metadata(&lock_path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "dev-dump cache lock must not be a symlink: {}",
                lock_path.display()
            ),
        ));
    }
    let lock = open_private_file(&lock_path, false)?;
    lock.lock_exclusive()?;

    // Migration is done under the same cross-process lock as writes. The old layout copied source
    // path segments into the cache tree; removing it both prevents stale name disclosure and makes
    // the version directory the only place future retention accounting has to inspect.
    remove_obsolete_layout(&dumps)?;
    let version = version_dir(root);
    ensure_private_directory(&version)?;

    let path = dump_path(root, key);
    let (tmp, mut output) = create_temp_file(&path)?;
    if let Err(error) = output
        .write_all(text.as_bytes())
        .and_then(|()| output.flush())
    {
        drop(output);
        let _ = fs::remove_file(&tmp);
        return Err(error);
    }
    drop(output);
    if let Err(error) = replace_file(&tmp, &path) {
        let _ = fs::remove_file(&tmp);
        return Err(error);
    }

    if let Err(error) = enforce_limits(&version, &path, max_files, max_cache_bytes) {
        // Do not let a directory containing undeletable stale entries grow by one file per
        // request. The write has completed, but returning its path would violate the advertised
        // cache-wide bound, so discard this new entry and surface failure to the caller.
        let _ = fs::remove_file(&path);
        return Err(error);
    }
    Ok(path)
}

/// `<root>/dumps/vN/<SHA-256(document identity)>.krusty.md`.
pub fn dump_path(root: &Path, key: &str) -> PathBuf {
    let mut hasher = Sha256::new();
    // Domain separation prevents a digest copied from an unrelated SHA-256 use from accidentally
    // becoming a valid dump identity if this cache is ever composed with another store.
    hasher.update(b"krusty-dev-dump\0");
    hasher.update(key.as_bytes());
    let digest = hasher.finalize();
    let mut name = String::with_capacity(digest.len() * 2 + ".krusty.md".len());
    for byte in digest {
        let _ = write!(name, "{byte:02x}");
    }
    name.push_str(".krusty.md");
    version_dir(root).join(name)
}

fn version_dir(root: &Path) -> PathBuf {
    root.join("dumps").join(format!("v{DUMP_FORMAT_VERSION}"))
}

/// Create a unique private temp file beside `path`.
///
/// `create_new` is important even though the counter is process-local: a crashed process may leave
/// a temp file behind and an operating system may later reuse its PID. Retrying another counter
/// value avoids truncating a file that another process could still own.
fn create_temp_file(path: &Path) -> io::Result<(PathBuf, File)> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    for _ in 0..64 {
        let count = COUNTER.fetch_add(1, Ordering::Relaxed);
        let tmp = tmp_path(path, count);
        match open_private_file(&tmp, true) {
            Ok(file) => return Ok((tmp, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not allocate a unique dev-dump temp file",
    ))
}

fn tmp_path(path: &Path, count: u64) -> PathBuf {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("dump");
    path.with_file_name(format!("{file_name}.{}-{count}.tmp", std::process::id()))
}

fn open_private_file(path: &Path, create_new: bool) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true);
    if create_new {
        options.create_new(true);
    } else {
        options.create(true);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path)?;
    set_private_file_permissions(&file)?;
    Ok(file)
}

fn ensure_private_directory(path: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)?;

    // Refuse a pre-existing symlink. Following one would let a cache path controlled by another
    // process redirect sensitive dump contents outside the directory the user selected.
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "dev-dump cache path is not a real directory: {}",
                path.display()
            ),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn set_private_file_permissions(file: &File) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

fn replace_file(tmp: &Path, path: &Path) -> io::Result<()> {
    // Unix rename replaces the destination atomically. Windows' standard-library rename refuses
    // an existing destination; the writers are still serialized by `.lock`, so removing it first
    // cannot let two complete writes interleave, although a reader may briefly observe no file.
    #[cfg(windows)]
    if path.exists() {
        fs::remove_file(path)?;
    }
    fs::rename(tmp, path)
}

fn remove_obsolete_layout(dumps: &Path) -> io::Result<()> {
    let current = format!("v{DUMP_FORMAT_VERSION}");
    for entry in fs::read_dir(dumps)? {
        let entry = entry?;
        let name = entry.file_name();
        if name == ".lock" || name == current.as_str() {
            continue;
        }
        remove_cache_entry(&entry.path(), entry.file_type()?)?;
    }
    Ok(())
}

fn remove_cache_entry(path: &Path, file_type: fs::FileType) -> io::Result<()> {
    if file_type.is_dir() && !file_type.is_symlink() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
}

fn enforce_limits(
    version: &Path,
    current: &Path,
    max_files: usize,
    max_bytes: u64,
) -> io::Result<()> {
    let mut entries = Vec::new();
    for entry in fs::read_dir(version)? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.ends_with(".tmp") {
            // No live writer can exist while the global lock is held, so every temp file here was
            // abandoned by a crashed process and is safe to remove.
            remove_cache_entry(&path, file_type)?;
            continue;
        }
        if !file_type.is_file() || !name.ends_with(".krusty.md") {
            continue;
        }
        let metadata = entry.metadata()?;
        let modified = metadata.modified().unwrap_or(UNIX_EPOCH);
        entries.push((path, modified, metadata.len()));
    }

    let mut total = entries
        .iter()
        .fold(0u64, |bytes, (_, _, size)| bytes.saturating_add(*size));
    // The just-written file is sorted last so enforcing the cache-wide budget does not make
    // `store` return a path it has already deleted.
    entries.sort_by_key(|(path, modified, _)| (path == current, *modified));
    let mut count = entries.len();
    for (path, _, size) in entries {
        if count <= max_files && total <= max_bytes {
            break;
        }
        if path == current {
            continue;
        }
        fs::remove_file(&path)?;
        count = count.saturating_sub(1);
        total = total.saturating_sub(size);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    fn scratch(name: &str) -> crate::project::TempTree {
        crate::project::TempTree::new(&format!("dump-{name}"))
    }

    #[test]
    fn the_path_is_stable_across_content_changes() {
        let tree = scratch("stable");
        let first = store(tree.root(), "file:///workspace/src/First.kt", "one").unwrap();
        let second = store(tree.root(), "file:///workspace/src/First.kt", "two").unwrap();

        assert_eq!(
            first, second,
            "the dump path must not move when content changes"
        );
        assert_eq!(fs::read_to_string(&second).unwrap(), "two");
    }

    #[test]
    fn cache_paths_are_opaque_fixed_size_markdown_names() {
        let tree = scratch("opaque");
        let key = "file:///workspace/src/ReadableName.kt";
        let path = store(tree.root(), key, "body").unwrap();
        let name = path.file_name().unwrap().to_string_lossy();

        assert!(path.starts_with(version_dir(tree.root())), "{path:?}");
        assert_eq!(name.len(), 64 + ".krusty.md".len());
        assert!(name.ends_with(".krusty.md"), "{name}");
        assert!(!path.to_string_lossy().contains("ReadableName"));
        assert!(!path.to_string_lossy().contains("workspace"));
    }

    #[test]
    fn arbitrary_keys_cannot_escape_or_alias_each_other() {
        let tree = scratch("identity");
        let traversal = store(tree.root(), "../../etc/passwd", "traversal").unwrap();
        let punctuation = store(tree.root(), "pkg:First.kt", "punctuation").unwrap();
        let literal_escape = store(tree.root(), "pkg_3aFirst.kt", "literal").unwrap();
        let huge = dump_path(tree.root(), &"x".repeat(100_000));

        assert!(
            traversal.starts_with(version_dir(tree.root())),
            "{traversal:?}"
        );
        assert_ne!(punctuation, literal_escape);
        assert_eq!(fs::read_to_string(punctuation).unwrap(), "punctuation");
        assert_eq!(fs::read_to_string(literal_escape).unwrap(), "literal");
        assert_eq!(
            huge.file_name().unwrap().len(),
            64 + ".krusty.md".len(),
            "untrusted key length must not become a filesystem component length"
        );
    }

    #[test]
    fn same_named_sources_use_the_full_document_identity() {
        let tree = scratch("same-name");
        let first = store(tree.root(), "file:///first/Shared.kt", "first").unwrap();
        let second = store(tree.root(), "file:///second/Shared.kt", "second").unwrap();

        assert_ne!(first, second);
        assert_eq!(fs::read_to_string(first).unwrap(), "first");
        assert_eq!(fs::read_to_string(second).unwrap(), "second");
    }

    #[test]
    fn temp_paths_are_unique_and_beside_the_final_path() {
        let tree = scratch("tmp-unique");
        let path = dump_path(tree.root(), "file:///workspace/src/First.kt");

        let first = tmp_path(&path, 1);
        let second = tmp_path(&path, 2);

        assert_ne!(first, second);
        assert_eq!(
            first.parent(),
            path.parent(),
            "rename must stay on one filesystem"
        );
    }

    #[test]
    fn concurrent_stores_never_interleave_or_leave_temp_files() {
        let tree = scratch("concurrent");
        let root = tree.root().to_path_buf();
        let text_a = "a".repeat(200_000);
        let text_b = "b".repeat(200_000);

        let root_a = root.clone();
        let text_a_thread = text_a.clone();
        let handle_a = std::thread::spawn(move || {
            store(&root_a, "file:///workspace/src/First.kt", &text_a_thread)
        });

        let root_b = root.clone();
        let text_b_thread = text_b.clone();
        let handle_b = std::thread::spawn(move || {
            store(&root_b, "file:///workspace/src/First.kt", &text_b_thread)
        });

        let path_a = handle_a.join().unwrap().unwrap();
        let path_b = handle_b.join().unwrap().unwrap();
        assert_eq!(path_a, path_b, "the key's path must stay stable");
        let final_text = fs::read_to_string(&path_a).unwrap();
        assert!(
            final_text == text_a || final_text == text_b,
            "final dump must hold one writer's complete text"
        );
        assert!(
            fs::read_dir(path_a.parent().unwrap())
                .unwrap()
                .all(|entry| !entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".tmp")),
            "a completed store must clean abandoned temp files"
        );
    }

    #[test]
    fn file_and_cache_limits_are_enforced() {
        let tree = scratch("limits");
        let first = store_with_limits(tree.root(), "one", "1111", 4, 2, 8).unwrap();
        let second = store_with_limits(tree.root(), "two", "2222", 4, 2, 8).unwrap();
        let third = store_with_limits(tree.root(), "three", "3333", 4, 2, 8).unwrap();

        assert!(third.exists(), "the just-written entry must be retained");
        assert!(
            !(first.exists() && second.exists()),
            "one of the equally old entries must be evicted"
        );
        assert_eq!(
            fs::read_dir(version_dir(tree.root()))
                .unwrap()
                .filter_map(Result::ok)
                .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "md"))
                .count(),
            2
        );
        let error = store_with_limits(tree.root(), "large", "12345", 4, 2, 8).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }

    #[cfg(unix)]
    #[test]
    fn dump_directory_and_files_are_private() {
        let tree = scratch("permissions");
        let path = store(tree.root(), "file:///workspace/src/First.kt", "body").unwrap();
        let directory_mode = fs::metadata(path.parent().unwrap())
            .unwrap()
            .permissions()
            .mode();
        let file_mode = fs::metadata(path).unwrap().permissions().mode();

        assert_eq!(directory_mode & 0o777, 0o700);
        assert_eq!(file_mode & 0o777, 0o600);
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_cannot_redirect_the_cache_lock() {
        use std::os::unix::fs::symlink;

        let tree = scratch("lock-symlink");
        let dumps = tree.root().join("dumps");
        fs::create_dir_all(&dumps).unwrap();
        let outside = tree.root().join("outside");
        fs::write(&outside, "unchanged").unwrap();
        symlink(&outside, dumps.join(".lock")).unwrap();

        let error = store(tree.root(), "file:///workspace/src/First.kt", "body").unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(fs::read_to_string(outside).unwrap(), "unchanged");
    }

    #[test]
    fn a_store_removes_the_old_path_revealing_layout() {
        let tree = scratch("migration");
        let legacy = tree.root().join("dumps/src/LeakedName.kt.krusty.md");
        fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        fs::write(&legacy, "old").unwrap();

        store(tree.root(), "file:///workspace/src/First.kt", "new").unwrap();

        assert!(!legacy.exists());
    }
}
