//! The content-addressed artifact store.
//!
//! Keyed by [`CacheKey`], which by construction covers every input that can change emitted bytes.
//! This module's job is narrower and entirely about integrity: **a partially written entry must
//! never read as a hit.**
//!
//! That is the store's version of the rule that governs the whole design. A crash, a full disk, or
//! a killed build in the middle of `put` must leave behind either a complete entry or nothing at
//! all. A half-written entry that reads as a hit would hand a dependent a jar missing classes, with
//! no diagnostic — the same silent-wrongness failure the cache key exists to prevent, arriving by a
//! different route.
//!
//! Three mechanisms enforce it:
//!
//! 1. **Write to a temporary directory, then rename into place.** `rename(2)` within a filesystem
//!    is atomic, so an entry directory either exists complete or does not exist.
//! 2. **`MANIFEST` is written last, inside the temporary directory.** Even if a future change made
//!    entry construction non-atomic, an entry without a manifest reads as a miss.
//! 3. **Every read verifies the manifest against the files**: count, length and content hash. A
//!    truncated or tampered file is a miss, not an error and not a hit.
//!
//! Layout, versioned so a format change cannot misread old entries — the same shape
//! `crates/krusty-lsp/src/deps_cache.rs` uses:
//!
//! ```text
//! <root>/v3/<key>/MANIFEST
//! <root>/v3/<key>/files/<artifact path>
//! ```

use std::io;
use std::path::{Component, Path, PathBuf};

use crate::abi::AbiFingerprint;
use crate::cache::CacheKey;
use crate::digest::{digest_bytes, Digest, Hasher};

/// Bumped whenever the on-disk shape or the manifest grammar changes. Entries under an older
/// version directory are simply never consulted.
pub const STORE_FORMAT_VERSION: u32 = 3;

/// One module's cached output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CachedModule {
    /// Emitted artifacts as `(target-relative path, bytes)`, in emission order.
    pub artifacts: Vec<(String, Vec<u8>)>,
    /// The module's ABI fingerprint, so a dependent's cache key can be computed from a cache hit
    /// without recompiling or even re-reading the artifacts.
    pub abi: AbiFingerprint,
}

impl CachedModule {
    pub fn total_bytes(&self) -> usize {
        self.artifacts.iter().map(|(_, bytes)| bytes.len()).sum()
    }
}

/// Why a lookup did not produce a usable entry. Every variant is a MISS, never an error: a damaged
/// cache must degrade to recompilation, never to a failed build.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MissReason {
    /// No entry directory for this key.
    Absent,
    /// The entry exists but has no manifest — an interrupted `put`, or a directory left by a
    /// crash between `mkdir` and `rename`.
    NoManifest,
    /// The manifest could not be parsed.
    MalformedManifest,
    /// A file named by the manifest is missing, the wrong length, or hashes differently.
    CorruptEntry,
}

/// A lookup distinguishes I/O failure from an ordinary cache miss without a nested anonymous type.
pub type StoreLookup = io::Result<Result<CachedModule, MissReason>>;

#[derive(Clone, Debug)]
pub struct ArtifactStore {
    versioned_root: PathBuf,
}

impl ArtifactStore {
    /// Open (creating if needed) a store under `root`. The version segment is added here, so
    /// callers pass a plain cache directory.
    pub fn open(root: impl AsRef<Path>) -> io::Result<Self> {
        let versioned_root = root.as_ref().join(format!("v{STORE_FORMAT_VERSION}"));
        std::fs::create_dir_all(&versioned_root)?;
        Ok(Self { versioned_root })
    }

    pub fn root(&self) -> &Path {
        &self.versioned_root
    }

    fn entry_dir(&self, key: CacheKey) -> PathBuf {
        self.versioned_root.join(key.to_string())
    }

    /// Look up a key. `Err` is reserved for genuine I/O faults on an otherwise healthy store; a
    /// damaged or absent entry is `Ok(Err(MissReason))`.
    pub fn get(&self, key: CacheKey) -> StoreLookup {
        let dir = self.entry_dir(key);
        if !dir.is_dir() {
            return Ok(Err(MissReason::Absent));
        }
        let manifest_path = dir.join("MANIFEST");
        let manifest = match std::fs::read_to_string(&manifest_path) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(Err(MissReason::NoManifest))
            }
            Err(error) => return Err(error),
        };

        let Some(parsed) = Manifest::parse(&manifest) else {
            return Ok(Err(MissReason::MalformedManifest));
        };

        let mut artifacts = Vec::with_capacity(parsed.files.len());
        for record in &parsed.files {
            let Some(relative) = safe_relative_path(&record.path) else {
                return Ok(Err(MissReason::MalformedManifest));
            };
            let bytes = match std::fs::read(dir.join("files").join(&relative)) {
                Ok(bytes) => bytes,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    return Ok(Err(MissReason::CorruptEntry))
                }
                Err(error) => return Err(error),
            };
            if bytes.len() != record.length || digest_bytes(&bytes) != record.content {
                return Ok(Err(MissReason::CorruptEntry));
            }
            artifacts.push((record.path.clone(), bytes));
        }

        Ok(Ok(CachedModule {
            artifacts,
            abi: parsed.abi,
        }))
    }

    /// Store an entry. Atomic: the entry becomes visible in one `rename`, or not at all.
    ///
    /// Storing a key that is already present is a no-op — two builds that agree on the key agree on
    /// the bytes, so there is nothing to choose between them.
    pub fn put(&self, key: CacheKey, entry: &CachedModule) -> io::Result<()> {
        let final_dir = self.entry_dir(key);
        if final_dir.is_dir() {
            // A HEALTHY entry under this key is authoritative: two builds that agree on the key
            // agree on the bytes, so there is nothing to choose between them.
            //
            // A DAMAGED one is different, and returning early on it was a trap: the read path
            // reports a miss, the driver recompiles, and then this early return refused to publish
            // the replacement — so the same corrupt entry caused a miss forever and the module was
            // recompiled on every single build. Quarantine it instead, then fall through and
            // publish. Quarantining rather than deleting keeps the evidence for diagnosis and
            // avoids racing a concurrent reader mid-read.
            match self.get(key) {
                Ok(Ok(_)) => return Ok(()),
                Ok(Err(_damaged)) => {
                    let quarantine = self
                        .versioned_root
                        .join(format!(".damaged-{key}-{}", unique_suffix()));
                    // If the rename loses a race, someone else already dealt with it.
                    if std::fs::rename(&final_dir, &quarantine).is_err() && final_dir.is_dir() {
                        return Ok(());
                    }
                }
                Err(error) => return Err(error),
            }
        }

        let staging = self.versioned_root.join(format!(
            ".staging-{}-{}-{}",
            std::process::id(),
            key,
            unique_suffix()
        ));
        // A previous crash may have left this exact name; start clean.
        let _ = std::fs::remove_dir_all(&staging);
        std::fs::create_dir_all(staging.join("files"))?;

        let mut manifest = Manifest {
            abi: entry.abi,
            files: Vec::with_capacity(entry.artifacts.len()),
        };
        for (path, bytes) in &entry.artifacts {
            let Some(relative) = safe_relative_path(path) else {
                let _ = std::fs::remove_dir_all(&staging);
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("artifact path escapes the entry directory: {path}"),
                ));
            };
            let destination = staging.join("files").join(&relative);
            if let Some(parent) = destination.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&destination, bytes)?;
            manifest.files.push(FileRecord {
                path: path.clone(),
                length: bytes.len(),
                content: digest_bytes(bytes),
            });
        }

        // Written last: an entry without a complete manifest can never be read as a hit.
        std::fs::write(staging.join("MANIFEST"), manifest.render())?;

        match std::fs::rename(&staging, &final_dir) {
            Ok(()) => Ok(()),
            Err(_) if final_dir.is_dir() => {
                // Another build won the race with an entry under the same key, so it holds the
                // same bytes. Drop ours.
                let _ = std::fs::remove_dir_all(&staging);
                Ok(())
            }
            Err(error) => {
                let _ = std::fs::remove_dir_all(&staging);
                Err(error)
            }
        }
    }

    /// Number of complete entries currently stored.
    pub fn len(&self) -> io::Result<usize> {
        let mut count = 0;
        for entry in std::fs::read_dir(&self.versioned_root)? {
            let entry = entry?;
            if entry.path().join("MANIFEST").is_file() {
                count += 1;
            }
        }
        Ok(count)
    }

    pub fn is_empty(&self) -> io::Result<bool> {
        Ok(self.len()? == 0)
    }

    /// Remove entries whose manifest has not been modified within `max_age`, plus any leftover
    /// staging and quarantined-damaged directories. Returns the number of entries removed.
    ///
    /// Deliberately simple: eviction policy is a real design question (size caps, LRU, shared
    /// caches) and belongs with the remote-caching work, which the proposal leaves open.
    pub fn gc_older_than(&self, max_age: std::time::Duration) -> io::Result<usize> {
        let now = std::time::SystemTime::now();
        let mut removed = 0;
        for entry in std::fs::read_dir(&self.versioned_root)? {
            let entry = entry?;
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with(".staging-") || name.starts_with(".damaged-") {
                let _ = std::fs::remove_dir_all(&path);
                continue;
            }
            let manifest = path.join("MANIFEST");
            let Ok(metadata) = std::fs::metadata(&manifest) else {
                // No manifest: an incomplete entry, never usable. Reclaim it.
                let _ = std::fs::remove_dir_all(&path);
                removed += 1;
                continue;
            };
            let Ok(modified) = metadata.modified() else {
                continue;
            };
            if now.duration_since(modified).unwrap_or_default() > max_age {
                std::fs::remove_dir_all(&path)?;
                removed += 1;
            }
        }
        Ok(removed)
    }
}

/// Reject an artifact path that would escape the entry directory. Artifact names come from a
/// backend rather than a user, but a store that writes wherever it is told is a bad store.
fn safe_relative_path(path: &str) -> Option<PathBuf> {
    // MANIFEST is deliberately line-oriented. A control character in an artifact name could
    // terminate or reshape its `file` record, so reject it at both write and read boundaries.
    if path.chars().any(char::is_control) {
        return None;
    }
    let candidate = Path::new(path);
    if candidate.is_absolute() {
        return None;
    }
    let mut out = PathBuf::new();
    for component in candidate.components() {
        match component {
            Component::Normal(part) => out.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    (!out.as_os_str().is_empty()).then_some(out)
}

/// Monotonic-enough suffix so two `put`s in one process cannot collide on a staging directory.
fn unique_suffix() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    nanos
        ^ COUNTER
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_mul(0x9e37_79b9_7f4a_7c15)
}

#[derive(Debug)]
struct FileRecord {
    path: String,
    length: usize,
    content: Digest,
}

#[derive(Debug)]
struct Manifest {
    abi: AbiFingerprint,
    files: Vec<FileRecord>,
}

impl Manifest {
    /// Line-oriented and hand-written: the crate takes no serialization dependency, and a format a
    /// human can read in a cache directory is worth more here than a compact one.
    fn render(&self) -> String {
        let mut out = format!(
            "version {STORE_FORMAT_VERSION}\nabi {}\ncount {}\n",
            self.abi,
            self.files.len()
        );
        for record in &self.files {
            out.push_str(&format!(
                "file {} {} {}\n",
                record.length, record.content, record.path
            ));
        }
        out.push_str(&format!("integrity {}\nend\n", self.integrity()));
        out
    }

    fn parse(text: &str) -> Option<Self> {
        let mut lines = text.lines();
        let version = lines.next()?.strip_prefix("version ")?;
        if version.trim() != STORE_FORMAT_VERSION.to_string() {
            return None;
        }
        let abi = Digest::parse_hex(lines.next()?.strip_prefix("abi ")?.trim())?;
        let count: usize = lines.next()?.strip_prefix("count ")?.trim().parse().ok()?;
        let mut files = Vec::with_capacity(count);
        for _ in 0..count {
            let line = lines.next()?;
            let rest = line.strip_prefix("file ")?;
            // `length content path`, where path may itself contain spaces.
            let (length, rest) = rest.split_once(' ')?;
            let (content, path) = rest.split_once(' ')?;
            files.push(FileRecord {
                path: path.to_string(),
                length: length.parse().ok()?,
                content: Digest::parse_hex(content)?,
            });
        }
        let integrity = Digest::parse_hex(lines.next()?.strip_prefix("integrity ")?.trim())?;
        if lines.next()? != "end" || lines.next().is_some() {
            return None;
        }
        let manifest = Self {
            abi: AbiFingerprint::from_digest(abi),
            files,
        };
        (manifest.integrity() == integrity).then_some(manifest)
    }

    fn integrity(&self) -> Digest {
        let mut hasher = Hasher::new();
        hasher.nested("abi", self.abi.digest());
        hasher.count("files", self.files.len());
        for record in &self.files {
            hasher.text("path", &record.path);
            hasher.field("length", &(record.length as u64).to_le_bytes());
            hasher.nested("content", record.content);
        }
        hasher.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch directory that removes itself.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "krusty-build-store-{tag}-{}-{}",
                std::process::id(),
                unique_suffix()
            ));
            std::fs::create_dir_all(&path).expect("create temp dir");
            Self(path)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn entry() -> CachedModule {
        CachedModule {
            artifacts: vec![
                (
                    "lib/Api.class".into(),
                    b"\xca\xfe\xba\xbe class one".to_vec(),
                ),
                ("META-INF/m.kotlin_module".into(), b"module bytes".to_vec()),
            ],
            abi: AbiFingerprint::from_digest(digest_bytes(b"abi-one")),
        }
    }

    fn key(value: u64) -> CacheKey {
        CacheKey::from_digest(digest_bytes(&value.to_le_bytes()))
    }

    #[test]
    fn a_stored_entry_round_trips_exactly() {
        let temp = TempDir::new("roundtrip");
        let store = ArtifactStore::open(temp.path()).expect("open");
        let original = entry();
        store.put(key(1), &original).expect("put");

        let loaded = store.get(key(1)).expect("get").expect("must be a hit");
        assert_eq!(
            loaded, original,
            "artifacts and ABI must round-trip exactly"
        );
        assert_eq!(store.len().expect("len"), 1);
    }

    #[test]
    fn an_absent_key_is_a_miss_not_an_error() {
        let temp = TempDir::new("absent");
        let store = ArtifactStore::open(temp.path()).expect("open");
        assert_eq!(store.get(key(7)).expect("get"), Err(MissReason::Absent));
    }

    /// The property the store exists for: an interrupted `put` must not read as a hit.
    #[test]
    fn an_entry_without_a_manifest_is_a_miss() {
        let temp = TempDir::new("nomanifest");
        let store = ArtifactStore::open(temp.path()).expect("open");
        store.put(key(2), &entry()).expect("put");

        std::fs::remove_file(store.root().join(key(2).to_string()).join("MANIFEST"))
            .expect("remove manifest");
        assert_eq!(
            store.get(key(2)).expect("get"),
            Err(MissReason::NoManifest),
            "an entry mid-write must never be served"
        );
    }

    #[test]
    fn a_truncated_artifact_is_a_miss() {
        let temp = TempDir::new("truncated");
        let store = ArtifactStore::open(temp.path()).expect("open");
        store.put(key(3), &entry()).expect("put");

        let victim = store
            .root()
            .join(key(3).to_string())
            .join("files")
            .join("lib")
            .join("Api.class");
        std::fs::write(&victim, b"short").expect("truncate");
        assert_eq!(
            store.get(key(3)).expect("get"),
            Err(MissReason::CorruptEntry),
            "the manifest's length and hash must catch a damaged file"
        );
    }

    #[test]
    fn a_same_length_tampered_artifact_is_a_miss() {
        let temp = TempDir::new("tampered");
        let store = ArtifactStore::open(temp.path()).expect("open");
        store.put(key(9), &entry()).expect("put");

        let victim = store
            .root()
            .join(key(9).to_string())
            .join("files")
            .join("META-INF")
            .join("m.kotlin_module");
        let original = std::fs::read(&victim).expect("read");
        let mut tampered = original.clone();
        tampered[0] ^= 0xff;
        assert_eq!(tampered.len(), original.len());
        std::fs::write(&victim, &tampered).expect("tamper");

        assert_eq!(
            store.get(key(9)).expect("get"),
            Err(MissReason::CorruptEntry),
            "length alone is not integrity; the content hash must catch this"
        );
    }

    #[test]
    fn a_missing_artifact_file_is_a_miss() {
        let temp = TempDir::new("missingfile");
        let store = ArtifactStore::open(temp.path()).expect("open");
        store.put(key(4), &entry()).expect("put");
        std::fs::remove_file(
            store
                .root()
                .join(key(4).to_string())
                .join("files")
                .join("lib")
                .join("Api.class"),
        )
        .expect("remove");
        assert_eq!(
            store.get(key(4)).expect("get"),
            Err(MissReason::CorruptEntry)
        );
    }

    #[test]
    fn a_malformed_manifest_is_a_miss() {
        let temp = TempDir::new("malformed");
        let store = ArtifactStore::open(temp.path()).expect("open");
        store.put(key(5), &entry()).expect("put");
        std::fs::write(
            store.root().join(key(5).to_string()).join("MANIFEST"),
            "not a manifest at all\n",
        )
        .expect("clobber");
        assert_eq!(
            store.get(key(5)).expect("get"),
            Err(MissReason::MalformedManifest)
        );
    }

    #[test]
    fn a_valid_manifest_prefix_cannot_be_accepted_after_truncation() {
        let temp = TempDir::new("manifest-prefix");
        let store = ArtifactStore::open(temp.path()).expect("open");
        store.put(key(22), &entry()).expect("put");
        let manifest_path = store.root().join(key(22).to_string()).join("MANIFEST");
        let text = std::fs::read_to_string(&manifest_path).expect("read");
        let valid_prefix = text.lines().take(4).collect::<Vec<_>>().join("\n");
        std::fs::write(&manifest_path, valid_prefix).expect("truncate at record boundary");
        assert_eq!(
            store.get(key(22)).expect("get"),
            Err(MissReason::MalformedManifest),
            "count, integrity, and end marker reject a syntactically valid record prefix"
        );
    }

    #[test]
    fn mutating_only_the_manifest_abi_breaks_integrity() {
        let temp = TempDir::new("manifest-abi");
        let store = ArtifactStore::open(temp.path()).expect("open");
        store.put(key(23), &entry()).expect("put");
        let manifest_path = store.root().join(key(23).to_string()).join("MANIFEST");
        let text = std::fs::read_to_string(&manifest_path).expect("read");
        let replacement = digest_bytes(b"different-abi").to_string();
        let mutated = text
            .lines()
            .map(|line| {
                if line.starts_with("abi ") {
                    format!("abi {replacement}")
                } else {
                    line.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(&manifest_path, mutated).expect("mutate ABI only");
        assert_eq!(
            store.get(key(23)).expect("get"),
            Err(MissReason::MalformedManifest),
            "the integrity digest covers the ABI as well as file records"
        );
    }

    /// A future format bump must not misread today's entries.
    #[test]
    fn a_foreign_format_version_is_a_miss() {
        let temp = TempDir::new("version");
        let store = ArtifactStore::open(temp.path()).expect("open");
        store.put(key(6), &entry()).expect("put");
        let manifest_path = store.root().join(key(6).to_string()).join("MANIFEST");
        let text = std::fs::read_to_string(&manifest_path).expect("read");
        std::fs::write(
            &manifest_path,
            text.replacen(&format!("version {STORE_FORMAT_VERSION}"), "version 999", 1),
        )
        .expect("write");
        assert_eq!(
            store.get(key(6)).expect("get"),
            Err(MissReason::MalformedManifest)
        );
    }

    #[test]
    fn artifact_paths_with_directories_nest_correctly() {
        let temp = TempDir::new("nested");
        let store = ArtifactStore::open(temp.path()).expect("open");
        store.put(key(8), &entry()).expect("put");
        assert!(store
            .root()
            .join(key(8).to_string())
            .join("files/META-INF/m.kotlin_module")
            .is_file());
    }

    #[test]
    fn an_escaping_artifact_path_is_refused() {
        let temp = TempDir::new("escape");
        let store = ArtifactStore::open(temp.path()).expect("open");
        let malicious = CachedModule {
            artifacts: vec![("../../escaped.class".into(), b"nope".to_vec())],
            abi: AbiFingerprint::from_digest(digest_bytes(b"abi-malicious")),
        };
        assert!(
            store.put(key(10), &malicious).is_err(),
            "a store must not write outside its entry directory"
        );
        assert!(
            !temp.path().join("escaped.class").exists(),
            "nothing may be written outside the store"
        );
    }

    #[test]
    fn artifact_paths_with_manifest_control_characters_are_refused() {
        let temp = TempDir::new("control-path");
        let store = ArtifactStore::open(temp.path()).expect("open");
        for (index, path) in [
            "lib/Line\nBreak.class",
            "lib/Carriage\rReturn.class",
            "lib/Tabbed\tName.class",
        ]
        .into_iter()
        .enumerate()
        {
            let unsafe_entry = CachedModule {
                artifacts: vec![(path.into(), b"nope".to_vec())],
                abi: AbiFingerprint::from_digest(digest_bytes(b"abi-control-path")),
            };
            let error = store
                .put(key(30 + index as u64), &unsafe_entry)
                .expect_err("a control character cannot enter the line-oriented manifest");
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput, "{path:?}");
            assert_eq!(
                store.get(key(30 + index as u64)).expect("readable store"),
                Err(MissReason::Absent),
                "a refused path must not publish any entry: {path:?}"
            );
        }
    }

    #[test]
    fn putting_the_same_key_twice_is_a_no_op() {
        let temp = TempDir::new("idempotent");
        let store = ArtifactStore::open(temp.path()).expect("open");
        store.put(key(11), &entry()).expect("first put");
        store.put(key(11), &entry()).expect("second put");
        assert_eq!(store.len().expect("len"), 1);
        assert!(store.get(key(11)).expect("get").is_ok());
    }

    #[test]
    fn gc_reclaims_incomplete_entries_and_leaves_fresh_ones() {
        let temp = TempDir::new("gc");
        let store = ArtifactStore::open(temp.path()).expect("open");
        store.put(key(12), &entry()).expect("put");

        // An entry a crash left without a manifest.
        let orphan = store.root().join("orphan");
        std::fs::create_dir_all(orphan.join("files")).expect("mkdir");
        // A staging directory a crash left behind.
        let staging = store.root().join(".staging-123-abc-1");
        std::fs::create_dir_all(&staging).expect("mkdir");

        let removed = store
            .gc_older_than(std::time::Duration::from_secs(3600))
            .expect("gc");
        assert_eq!(removed, 1, "the manifest-less orphan is reclaimed");
        assert!(!staging.exists(), "staging leftovers are swept");
        assert!(
            store.get(key(12)).expect("get").is_ok(),
            "a fresh, complete entry survives"
        );
    }

    /// Finding: `put` returned early whenever the entry directory existed, so a DAMAGED entry was
    /// never replaced — the read path reported a miss forever and the module recompiled on every
    /// build. The damaged entry must be quarantined and the replacement published.
    #[test]
    fn a_damaged_entry_is_replaced_rather_than_missing_forever() {
        let temp = TempDir::new("repair");
        let store = ArtifactStore::open(temp.path()).expect("open");
        store.put(key(20), &entry()).expect("put");

        // Corrupt it exactly as an interrupted write would.
        std::fs::remove_file(store.root().join(key(20).to_string()).join("MANIFEST"))
            .expect("damage");
        assert_eq!(
            store.get(key(20)).expect("get"),
            Err(MissReason::NoManifest)
        );

        // A rebuild republishes the same key.
        store.put(key(20), &entry()).expect("republish");
        assert_eq!(
            store.get(key(20)).expect("get").expect("must now be a hit"),
            entry(),
            "the replacement must be readable; otherwise this key misses forever"
        );
    }

    #[test]
    fn a_healthy_entry_is_not_disturbed_by_republishing() {
        let temp = TempDir::new("republish-healthy");
        let store = ArtifactStore::open(temp.path()).expect("open");
        store.put(key(21), &entry()).expect("put");
        let before =
            std::fs::read_to_string(store.root().join(key(21).to_string()).join("MANIFEST"))
                .expect("read manifest");

        store.put(key(21), &entry()).expect("republish");
        let after =
            std::fs::read_to_string(store.root().join(key(21).to_string()).join("MANIFEST"))
                .expect("read manifest");
        assert_eq!(
            before, after,
            "a healthy entry is authoritative and untouched"
        );
        assert_eq!(
            store.len().expect("len"),
            1,
            "no quarantine directory was made"
        );
    }

    #[test]
    fn gc_reclaims_quarantined_damaged_entries() {
        let temp = TempDir::new("gc-damaged");
        let store = ArtifactStore::open(temp.path()).expect("open");
        let quarantined = store.root().join(".damaged-deadbeef-1");
        std::fs::create_dir_all(quarantined.join("files")).expect("mkdir");
        store
            .gc_older_than(std::time::Duration::from_secs(3600))
            .expect("gc");
        assert!(!quarantined.exists(), "quarantined entries are swept by gc");
    }

    #[test]
    fn the_store_root_is_version_segmented() {
        let temp = TempDir::new("versioned");
        let store = ArtifactStore::open(temp.path()).expect("open");
        assert!(store.root().ends_with(format!("v{STORE_FORMAT_VERSION}")));
    }
}
