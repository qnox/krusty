//! Cached class-name listings, one file per jar.
//!
//! Reading the class names out of a project's jars is 63% of what the dependency index costs to
//! build — 442 ms of 706 ms over 150 jars — and it is the same work in every workspace that
//! resolves the same artifact. Cached, that phase costs 11 ms: a 40x saving on the part of the
//! build that can be shared, and 2.6x on the whole.
//!
//! Keyed by path, size and modification time rather than by jar content. Content is the exact key
//! and would be the obvious choice, but it has to be read to be known: hashing those same 150 jars
//! (221 MB) costs 409 ms, which is the entire saving. A `stat` costs 0.69 ms. The key is written
//! into the cached file and checked on load, so a hash collision yields a miss rather than another
//! jar's classes.
//!
//! Only files are cached. A directory on the classpath can change without anything in its own
//! metadata moving, so it is read every time.
//!
//! Every way a cached file can be wrong — missing, truncated, corrupt, not UTF-8, written by a
//! newer version, belonging to another jar — ends at the same place: no answer, and the caller
//! reads the jar. A cache is an optimisation, and a broken one is only ever allowed to cost time.
//! Writing is best effort for the same reason: a cache directory that cannot be created or written
//! makes for a slower start, not a failure.
//!
//! What is cached is the jar's *raw* entry names, before anything is filtered or parsed out of
//! them. That is what keeps the format version honest: deciding that some entry is not worth
//! indexing changes the index, not the file, so a change of that kind cannot leave a stale cache
//! behind it. The version guards the file layout alone, and it is carried in the managed entry's
//! file name as well as its header, so an old layout misses and a hash collision cannot pass for a
//! hit. Managed entries share the rendered-source cache's version root and access marker; its
//! global age/size GC and `cache clean` operation therefore bound listings too.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use fs2::FileExt;

use crate::project::fingerprint::Hasher;

/// Bumped when the file layout changes, so an old entry misses instead of being misread.
const DEPENDENCY_CACHE_FORMAT_VERSION: u32 = 1;

/// What identifies a jar without reading it.
struct CacheKey {
    bytes: u64,
    modified_nanos: u128,
}

fn cache_key(jar: &Path) -> Option<CacheKey> {
    let metadata = fs::metadata(jar).ok()?;
    if !metadata.is_file() {
        return None;
    }
    let modified = metadata.modified().ok()?;
    Some(CacheKey {
        bytes: metadata.len(),
        modified_nanos: modified
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_nanos(),
    })
}

fn cache_entry_dir(cache_root: &Path, jar: &Path, key: &CacheKey) -> PathBuf {
    let mut hasher = Hasher::default();
    hasher.write_str("dependency-classes");
    hasher.write_str(&jar.to_string_lossy());
    hasher.write(&key.bytes.to_le_bytes());
    hasher.write(&key.modified_nanos.to_le_bytes());
    crate::deps_cache::managed_entry_dir(
        cache_root,
        &format!("classes-{:016x}", hasher.finish().as_u64()),
    )
}

fn cache_path(cache_root: &Path, jar: &Path, key: &CacheKey) -> PathBuf {
    cache_entry_dir(cache_root, jar, key).join(format!(
        "listing-v{DEPENDENCY_CACHE_FORMAT_VERSION}.classes"
    ))
}

/// The header a cached listing carries, so the key is verified rather than trusted.
///
/// It records the number of names as well as the key. A rename is atomic but the bytes it publishes
/// need not be durable: a crash between write and flush can leave a file whose header is intact and
/// whose body stops early, and the jar's key never changes again, so a short listing would be
/// permanent. The count turns that into a miss.
fn header(jar: &Path, key: &CacheKey, names: usize) -> String {
    format!(
        "{DEPENDENCY_CACHE_FORMAT_VERSION} {} {} {names} {}",
        key.bytes,
        key.modified_nanos,
        jar.to_string_lossy()
    )
}

/// Class names cached for `jar`, or `None` when nothing valid is cached for it.
pub fn load(cache_root: &Path, jar: &Path) -> Option<Vec<String>> {
    // Share the dependency-source cache lock so age/size GC and `cache clean` cannot remove this
    // managed entry while it is being read.
    let global = crate::deps_cache::global_lock(cache_root).ok()?;
    FileExt::lock_shared(&global).ok()?;
    let key = cache_key(jar)?;
    let text = fs::read_to_string(cache_path(cache_root, jar, &key)).ok()?;
    let mut lines = text.lines();
    let header_line = lines.next()?;
    let names = lines.map(str::to_string).collect::<Vec<_>>();
    // A key collision must miss rather than answer with another jar's classes, and a body that
    // stops short of what the header promises must miss rather than answer with part of one.
    if header_line != header(jar, &key, names.len()) {
        return None;
    }
    let _ = crate::deps_cache::touch_entry(&cache_entry_dir(cache_root, jar, &key));
    Some(names)
}

/// Cache `names` as the class listing for `jar`.
///
/// Best effort: a cache that cannot be written is a slower start, not a failure, so the caller is
/// told nothing it would have to handle.
pub fn store(cache_root: &Path, jar: &Path, names: &[String]) {
    let Ok(global) = crate::deps_cache::global_lock(cache_root) else {
        return;
    };
    if FileExt::lock_shared(&global).is_err() {
        return;
    }
    let Some(key) = cache_key(jar) else {
        return;
    };
    let path = cache_path(cache_root, jar, &key);
    if write_atomically(&path, &header(jar, &key, names.len()), names).is_ok() {
        let _ = crate::deps_cache::touch_entry(&cache_entry_dir(cache_root, jar, &key));
    }
}

fn write_atomically(path: &Path, header: &str, names: &[String]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut text =
        String::with_capacity(header.len() + names.iter().map(|n| n.len() + 1).sum::<usize>() + 1);
    text.push_str(header);
    text.push('\n');
    for name in names {
        text.push_str(name);
        text.push('\n');
    }
    // Written beside the target and renamed, so a reader never sees half a listing -- several
    // workspaces share this directory and may be writing the same jar at once.
    // The temporary name carries the thread as well as the process: several jars are written under
    // one cache root, and two threads writing the same jar would otherwise share a scratch file.
    let temporary = path.with_extension(format!(
        "classes.tmp{}.{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    fs::write(&temporary, text)?;
    fs::rename(&temporary, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "krusty-dep-cache-{label}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).ok();
        }
    }

    fn jar_like(directory: &Path, name: &str, contents: &str) -> PathBuf {
        let path = directory.join(name);
        fs::write(&path, contents).unwrap();
        path
    }

    #[test]
    fn a_stored_listing_is_read_back() {
        let temp = TempDir::new("roundtrip");
        let jar = jar_like(&temp.0, "library.jar", "jar bytes");
        let names = vec![
            "kotlin/collections/AbstractList".to_string(),
            "java/util/Map$Entry".to_string(),
        ];

        store(&temp.0, &jar, &names);

        assert_eq!(load(&temp.0, &jar), Some(names));
    }

    #[test]
    fn stored_listings_participate_in_shared_gc_and_ordinary_clean() {
        let temp = TempDir::new("managed-lifecycle");
        let cache = temp.0.join("cache");
        let jar = jar_like(&temp.0, "library.jar", "jar bytes");
        let names = vec!["demo/Managed".to_string()];
        store(&cache, &jar, &names);
        assert_eq!(load(&cache, &jar), Some(names.clone()));

        // A far-future clock makes the just-touched entry old without sleeping or mutating its
        // timestamp. The shared source-cache collector must see and evict the class listing.
        let stats = crate::deps_cache::gc(&cache, 0, u64::MAX, u64::MAX).unwrap();
        assert_eq!(stats.evicted, 1);
        assert_eq!(load(&cache, &jar), None);

        store(&cache, &jar, &names);
        assert_eq!(load(&cache, &jar), Some(names));
        crate::deps_cache::clean(&cache, false).unwrap();
        assert_eq!(load(&cache, &jar), None);
    }

    #[test]
    fn an_uncached_jar_misses() {
        let temp = TempDir::new("miss");
        let jar = jar_like(&temp.0, "library.jar", "jar bytes");

        assert_eq!(load(&temp.0, &jar), None);
    }

    #[test]
    fn a_rewritten_jar_misses_rather_than_serving_its_old_classes() {
        let temp = TempDir::new("rewritten");
        let jar = jar_like(&temp.0, "library.jar", "jar bytes");
        store(&temp.0, &jar, &["demo/Old".to_string()]);

        // A rebuilt jar of a different size is a different jar, whatever its path says.
        jar_like(&temp.0, "library.jar", "different jar bytes");

        assert_eq!(
            load(&temp.0, &jar),
            None,
            "a changed jar must not answer with what the old one declared"
        );
    }

    #[test]
    fn two_jars_do_not_read_each_others_listings() {
        let temp = TempDir::new("distinct");
        let first = jar_like(&temp.0, "first.jar", "same bytes");
        let second = jar_like(&temp.0, "second.jar", "same bytes");
        store(&temp.0, &first, &["demo/First".to_string()]);

        // Identical size and near-identical mtime: the path is part of the key, and the header is
        // checked, so the second jar cannot pick up the first one's listing.
        assert_eq!(load(&temp.0, &second), None);
        assert_eq!(load(&temp.0, &first), Some(vec!["demo/First".to_string()]));
    }

    #[test]
    fn a_directory_on_the_classpath_is_never_cached() {
        let temp = TempDir::new("directory");
        let directory = temp.0.join("classes");
        fs::create_dir_all(&directory).unwrap();

        // Its contents can change without its own metadata moving, so there is no key that would
        // stay honest.
        store(&temp.0, &directory, &["demo/Compiled".to_string()]);

        assert_eq!(load(&temp.0, &directory), None);
    }

    #[test]
    fn a_truncated_listing_misses_rather_than_reading_as_a_short_one() {
        let temp = TempDir::new("truncated");
        let jar = jar_like(&temp.0, "library.jar", "jar bytes");
        let names = (0..50)
            .map(|index| format!("demo/Type{index}"))
            .collect::<Vec<_>>();
        store(&temp.0, &jar, &names);
        let key = cache_key(&jar).unwrap();
        let path = cache_path(&temp.0, &jar, &key);
        let whole = fs::read_to_string(&path).unwrap();
        fs::write(&path, &whole[..whole.len() / 3]).unwrap();

        // A rename is atomic; the bytes it publishes need not be durable. A half-written listing
        // whose jar never changes again would otherwise be permanent.
        assert_eq!(load(&temp.0, &jar), None);
    }

    #[test]
    fn a_corrupt_cache_file_misses_so_the_jar_is_read_again() {
        let temp = TempDir::new("corrupt");
        let jar = jar_like(&temp.0, "library.jar", "jar bytes");
        store(&temp.0, &jar, &["demo/Type".to_string()]);
        let key = cache_key(&jar).unwrap();
        let path = cache_path(&temp.0, &jar, &key);

        // Every way a cached file can be wrong has to end at the same place: no answer, so the
        // caller reads the jar. A cache is an optimisation, and a broken one is only ever allowed
        // to cost time.
        for damage in ["", "garbage", "\n\n\n", "1 0 0 0 /elsewhere.jar\ndemo/Type"] {
            fs::write(&path, damage).unwrap();
            assert_eq!(load(&temp.0, &jar), None, "accepted {damage:?}");
        }

        // Not even valid UTF-8.
        fs::write(&path, [0xff, 0xfe, 0x00, 0x01]).unwrap();
        assert_eq!(load(&temp.0, &jar), None);

        // A directory where the file should be.
        fs::remove_file(&path).unwrap();
        fs::create_dir_all(&path).unwrap();
        assert_eq!(load(&temp.0, &jar), None);
    }

    #[test]
    fn an_unwritable_cache_root_is_not_an_error() {
        let temp = TempDir::new("unwritable");
        let jar = jar_like(&temp.0, "library.jar", "jar bytes");
        // The root is a file, so no directory can be created under it. Storing must stay silent:
        // a cache that cannot be written is a slower start, not a failure.
        let root = jar_like(&temp.0, "not-a-directory", "");

        store(&root, &jar, &["demo/Type".to_string()]);

        assert_eq!(load(&root, &jar), None);
    }

    #[test]
    fn an_empty_listing_round_trips_as_empty_rather_than_as_a_miss() {
        let temp = TempDir::new("empty");
        let jar = jar_like(&temp.0, "resources.jar", "jar bytes");

        // A jar of resources declares no classes, and that is worth caching: re-reading it every
        // time to learn nothing is exactly what this avoids.
        store(&temp.0, &jar, &[]);

        assert_eq!(load(&temp.0, &jar), Some(Vec::new()));
    }
}
