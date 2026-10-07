//! Kotlinc class-file dumps for byte-equality checks.
//!
//! A check compares krusty against a dump recorded from kotlinc for this fixture, and runs kotlinc
//! only when the dump is missing or its fingerprint no longer matches the fixture. A release
//! (`2.4.20`, `2.4.20-release-482`) and an RC tag (`2.4.20-RC`, `2.4.20-RC2`) may read and write
//! dumps. A snapshot, dev, or beta build never does: that version string is not an immutable
//! artifact.
//!
//! Each recording belongs to one exact compiler version. Release and RC channels remain separate.
//! The bytes live in one zlib archive under the class-dump cache
//! (`KRUSTY_CLASS_DUMP_DIR`, or `target/cache/class-dumps`), not in the repository. Identical
//! outputs are stored once inside it. A run keeps new dumps in memory and writes the archive
//! once, when the process exits. A text index in that archive records the exact compiler version
//! for each dump.
//!
//! `KRUSTY_RECORD=1` or `KRUSTY_RECORD_CLASS_DUMPS=1` ignores a stored dump and recompiles. A
//! release or RC with no matching dump fails locally instead of compiling. CI compiles that
//! missing test with kotlinc instead of failing. Master stores the new recording and publishes
//! that job's archive; a pull request leaves the restored archive unchanged. A restored entry is
//! replayed, so a run whose tests are already recorded does not call kotlinc. Every recorded run
//! keeps its exit code and kotlinc diagnostics, whether the build succeeded or failed, so a later
//! assert replays them. A class dump that has no exit code or diagnostics fails an assert that
//! needs them unless CI is allowed to compile the missing entry live. A snapshot, dev, or beta
//! build still compiles, because that version is not an immutable artifact and never reads or
//! writes the archive.
//!
//! The lookup fingerprint covers source bytes, target, flags, ambient selected-toolchain content,
//! and explicit classpath input identities; it is distinct from the blob id that hashes the
//! recorded compiler output. Entries proven to belong to the selected Kotlin distribution also use
//! their logical distribution path. An explicit selected JDK image uses its kind, exact content
//! identity, and `release` label. Other classpath entries contribute their bytes.
//!
//! An archive recorded before those toolchain rows is still replayed. Its index may name an open
//! range (`2.4.20..`); that line is the recording's own release, not a license for a later
//! compiler to reuse the bytes. A miss on the current fingerprint tries the earlier classpath
//! identity, which hashed each explicit entry and did not add a selected-installation row. A file
//! that does not parse is replaced when this process publishes, instead of being left in place for
//! every later run to miss.
//!
//! The archive is read at runtime and is not compiled into the test binary.

mod fingerprint;

pub use fingerprint::class_dump_inputs;
pub(crate) use fingerprint::fingerprint_parts;
use fingerprint::{
    basename, classpath_content_fingerprint, fnv64, legacy_classpath_fingerprint,
    normalize_invocation_flag,
};
#[cfg(test)]
use fingerprint::{
    class_dump_inputs_with_platform, classpath_content_fingerprint_with_platform, hash_tree,
    normalize_args, selected_content_hash_count,
};

use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::SystemTime;

use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;
use flate2::Compression;
use krusty::kotlin_version::KotlinVersion;

/// The published compiler identity, when this process's kotlinc is a release or an RC tag.
///
/// `None` for a missing dist, a snapshot, a dev build, a beta, or any other non-release string.
/// The value is `build.txt`'s first line, unchanged. A release build and `release-N` of the same
/// version share one exact release slot, while RC tags of that version share a separate exact slot.
pub fn published_compiler_id() -> Option<String> {
    static ID: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    ID.get_or_init(|| {
        let lib = krusty::toolchain::kotlinc_lib_dir()?;
        let text = std::fs::read_to_string(lib.parent()?.join("build.txt")).ok()?;
        let line = text.lines().next()?.trim();
        cacheable_build_identity(line)
    })
    .clone()
}

/// `build.txt` text that identifies one immutable release or RC build.
///
/// The returned string is the trimmed text itself. Anything else — a snapshot, a `dev` build, a
/// beta — is `None`.
pub fn cacheable_build_identity(text: &str) -> Option<String> {
    let text = text.trim();
    if text.is_empty()
        || !text
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'.' || byte == b'-')
    {
        return None;
    }
    let (version, rest) = match text.split_once('-') {
        Some((version, rest)) => (version, rest),
        None => (text, ""),
    };
    if !is_release_number(version) {
        return None;
    }
    if rest.is_empty() || is_release_build(rest) || is_rc(rest) {
        Some(text.to_string())
    } else {
        None
    }
}

fn is_release_number(text: &str) -> bool {
    let mut parts = text.split('.');
    (0..3).all(|_| {
        parts
            .next()
            .is_some_and(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
    }) && parts.next().is_none()
}

fn is_digits(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit())
}

fn is_release_build(rest: &str) -> bool {
    rest.strip_prefix("release-").is_some_and(is_digits)
}

/// `RC`, `RC2`, `RC-137`, `RC2-release-15`, `RC2-15`.
fn is_rc(rest: &str) -> bool {
    let Some(rest) = rest.strip_prefix("RC") else {
        return false;
    };
    if rest.is_empty() {
        return true;
    }
    if let Some(tail) = rest.strip_prefix('-') {
        return is_digits(tail) || is_release_build(tail);
    }
    let (digits, after) = split_leading_digits(rest);
    if digits.is_empty() {
        return false;
    }
    if after.is_empty() {
        return true;
    }
    let Some(after) = after.strip_prefix('-') else {
        return false;
    };
    is_digits(after) || is_release_build(after)
}

fn split_leading_digits(text: &str) -> (&str, &str) {
    let end = text
        .bytes()
        .position(|byte| !byte.is_ascii_digit())
        .unwrap_or(text.len());
    text.split_at(end)
}

/// Kotlinc's bytes for each of `classes`, from the recorded dump or from `compile`.
///
/// `compile` runs when recording is forced, when CI explicitly permits a read-only compile for a
/// missing entry, or for a compiler that is not a release or RC. Otherwise, a release or RC with no
/// matching dump fails the test. `None` from `compile` yields `None`. A non-release compiler always
/// takes `compile` and does not touch the archive.
pub fn kotlinc_class_dumps(
    stem: &str,
    jvm_target: &str,
    variant: &str,
    fingerprint: u128,
    legacy_fingerprint: u128,
    classes: &[&str],
    compile: impl FnOnce() -> Option<BTreeMap<String, Vec<u8>>>,
) -> Option<Vec<Vec<u8>>> {
    let (module, case) = running_test();
    let suffix = classes_suffix(classes);
    let produced = recall(
        Recall {
            root: &dumps_root(),
            module: &module,
            key: &entry_key(&case, stem, jvm_target, variant, &suffix),
            compiler: compiler_dump_version(),
            fingerprint,
            legacy_fingerprint,
            force: record_forced(),
            compile_missing: compile_missing_allowed(),
            write: ci_allows_write(),
        },
        |hit| classes.iter().all(|class| hit.contains_key(*class)),
        compile,
    )?;
    classes
        .iter()
        .map(|class| produced.get(*class).cloned())
        .collect()
}

/// Every class kotlinc emits for one fixture, from the recorded tree or from `compile`.
///
/// `compile` returns internal class names (no `.class` suffix). A non-release compiler always
/// takes `compile` and does not touch the dump directory.
#[allow(dead_code)] // bytecode comparison lives in the e2e crate, not conformance.
pub fn kotlinc_class_tree(
    stem: &str,
    jvm_target: &str,
    variant: &str,
    fingerprint: u128,
    legacy_fingerprint: u128,
    compile: impl FnOnce() -> Option<BTreeMap<String, Vec<u8>>>,
) -> Option<BTreeMap<String, Vec<u8>>> {
    let (module, case) = running_test();
    let files = recall(
        Recall {
            root: &dumps_root(),
            module: &module,
            key: &entry_key(&case, stem, jvm_target, variant, "#tree"),
            compiler: compiler_dump_version(),
            fingerprint,
            legacy_fingerprint,
            force: record_forced(),
            compile_missing: compile_missing_allowed(),
            write: ci_allows_write(),
        },
        |_| true,
        || {
            let classes = compile()?;
            Some(
                classes
                    .into_iter()
                    .map(|(name, bytes)| (format!("{name}.class"), bytes))
                    .collect(),
            )
        },
    )?;
    Some(
        files
            .into_iter()
            .map(|(name, bytes)| {
                (
                    name.strip_suffix(".class").unwrap_or(&name).to_string(),
                    bytes,
                )
            })
            .collect(),
    )
}

/// A dump shared by every test that builds the same kotlinc library output.
///
/// `None` when this compiler is not a release or RC, when recording is forced, or when no exact
/// compiler-version entry exists at `fingerprint`.
pub fn load_shared_files(slot: &str, fingerprint: u128) -> Option<BTreeMap<String, Vec<u8>>> {
    if record_forced() {
        return None;
    }
    let compiler = compiler_dump_version()?;
    load_files(&dumps_root(), "_libs", slot, compiler, fingerprint)
}

/// Record `files` (relative path → bytes, including `META-INF` entries) for [`load_shared_files`].
///
/// A no-op for a snapshot, dev, or beta compiler, and under read-only CI.
pub fn store_shared_files(slot: &str, fingerprint: u128, files: &BTreeMap<String, Vec<u8>>) {
    let Some(compiler) = compiler_dump_version() else {
        return;
    };
    if !ci_allows_write() {
        return;
    }
    store_files(&dumps_root(), "_libs", slot, compiler, fingerprint, files);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Channel {
    Release,
    Rc,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DumpVersion {
    version: KotlinVersion,
    channel: Channel,
}

fn compiler_dump_version() -> Option<DumpVersion> {
    parse_dump_version(&published_compiler_id()?)
}

fn parse_dump_version(identity: &str) -> Option<DumpVersion> {
    let (version, rest) = match identity.split_once('-') {
        Some((version, rest)) => (version, rest),
        None => (identity, ""),
    };
    let version = KotlinVersion::parse(version)?;
    let channel = if rest.starts_with("RC") {
        Channel::Rc
    } else {
        Channel::Release
    };
    Some(DumpVersion { version, channel })
}

#[derive(Clone, Copy)]
struct Recall<'a> {
    root: &'a Path,
    module: &'a str,
    key: &'a str,
    compiler: Option<DumpVersion>,
    fingerprint: u128,
    /// Tried only after `fingerprint` misses. Equal to `fingerprint` when there is no earlier key.
    legacy_fingerprint: u128,
    force: bool,
    compile_missing: bool,
    write: bool,
}

fn recall(
    query: Recall<'_>,
    accept: impl Fn(&BTreeMap<String, Vec<u8>>) -> bool,
    compile: impl FnOnce() -> Option<BTreeMap<String, Vec<u8>>>,
) -> Option<BTreeMap<String, Vec<u8>>> {
    let Some(compiler) = query.compiler else {
        return compile();
    };
    if !query.force {
        if let Some(hit) = load_fingerprint(&query, compiler, query.fingerprint, &accept) {
            return Some(hit);
        }
        if query.legacy_fingerprint != query.fingerprint {
            if let Some(hit) = load_fingerprint(&query, compiler, query.legacy_fingerprint, &accept)
            {
                return Some(hit);
            }
        }
        if !query.compile_missing {
            refuse_missing_dump(&format!(
                "module {} key {} fingerprint {:032x}",
                query.module, query.key, query.fingerprint
            ));
        }
    }
    let produced = compile()?;
    if query.write {
        store_files(
            query.root,
            query.module,
            query.key,
            compiler,
            query.fingerprint,
            &produced,
        );
    }
    Some(produced)
}

fn load_fingerprint(
    query: &Recall<'_>,
    compiler: DumpVersion,
    fingerprint: u128,
    accept: &impl Fn(&BTreeMap<String, Vec<u8>>) -> bool,
) -> Option<BTreeMap<String, Vec<u8>>> {
    load_files(query.root, query.module, query.key, compiler, fingerprint).filter(|hit| accept(hit))
}

fn refuse_missing_dump(detail: &str) -> ! {
    let compiler = published_compiler_id().unwrap_or_else(|| "this release".to_string());
    panic!(
        "the recorded-byte cache has no class dump for {detail} under kotlinc {compiler}. \
         This run does not compile with kotlinc for a release or RC that already uses the archive. \
         Refresh the master GitHub cache with KRUSTY_RECORD_CLASS_DUMPS=1."
    );
}

fn entry_key(case: &str, stem: &str, jvm_target: &str, variant: &str, suffix: &str) -> String {
    format!(
        "{case}|{stem}|{jvm_target}|{}{suffix}",
        variant_component(variant)
    )
}

/// The requested class set is part of the key. Callers store only those classes, so two requests
/// that share a stem must not overwrite each other's bytes.
fn classes_suffix(classes: &[&str]) -> String {
    let mut names: Vec<&str> = classes.to_vec();
    names.sort_unstable();
    format!("#{}", names.join(","))
}

fn variant_component(variant: &str) -> String {
    if variant.is_empty() {
        "plain".to_string()
    } else {
        format!("{:016x}", fnv64(0xcbf29ce484222325, variant.as_bytes()))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RecordedEntry {
    version: KotlinVersion,
    channel: Channel,
    fingerprint: u128,
    blob: u128,
}

fn load_files(
    root: &Path,
    module: &str,
    key: &str,
    compiler: DumpVersion,
    fingerprint: u128,
) -> Option<BTreeMap<String, Vec<u8>>> {
    let path = archive_path(root);
    let mut cache = dump_cache().lock().expect("class-dump cache");
    let archive = cached_archive(&path, &mut cache)?;
    let entries = archive.modules.get(&sanitize(module))?.get(key)?;
    let entry = entries.iter().find(|entry| {
        entry.channel == compiler.channel
            && entry.version == compiler.version
            && entry.fingerprint == fingerprint
    })?;
    let raw = archive.blob(entry.blob)?.to_vec();
    drop(cache);
    decode_raw(&raw)
}

fn store_files(
    root: &Path,
    module: &str,
    key: &str,
    compiler: DumpVersion,
    fingerprint: u128,
    files: &BTreeMap<String, Vec<u8>>,
) {
    let raw = encode_raw(files);
    let blob = fingerprint_parts(&[&raw]);
    let path = archive_path(root);
    let parent = path.parent().expect("class-dump directory");
    std::fs::create_dir_all(parent).expect("create class-dump directory");
    let mut cache = dump_cache().lock().expect("class-dump cache");
    reconcile(&path, &mut cache);
    let slot = cache.entry(path).or_insert_with(CacheSlot::empty);
    let module = sanitize(module);
    // One test can record two fixtures under one key. Keep each fingerprint's entries; replacing
    // the version slot would drop the earlier fixture and the next run would miss it.
    let (updated, unchanged) = {
        let current = slot
            .archive
            .modules
            .get(&module)
            .and_then(|entries| entries.get(key))
            .cloned()
            .unwrap_or_default();
        let mut updated = current;
        updated.retain(|entry| {
            entry.channel != compiler.channel
                || entry.version != compiler.version
                || entry.fingerprint != fingerprint
        });
        updated.push(RecordedEntry {
            version: compiler.version,
            channel: compiler.channel,
            fingerprint,
            blob,
        });
        updated.sort_by_key(|entry| {
            (
                entry.channel == Channel::Rc,
                entry.version,
                entry.fingerprint,
            )
        });
        let unchanged = slot
            .archive
            .modules
            .get(&module)
            .and_then(|entries| entries.get(key))
            == Some(&updated)
            && slot.archive.blobs.contains_key(&blob);
        (updated, unchanged)
    };
    if unchanged {
        return;
    }
    slot.archive
        .modules
        .entry(module.clone())
        .or_default()
        .insert(key.to_string(), updated);
    slot.archive.insert_blob(blob, raw);
    let pending = PendingDump {
        module,
        key: key.to_string(),
        compiler,
        fingerprint,
        blob,
    };
    // Only the last publication of one exact version/fingerprint matters. Coalescing it keeps the
    // replay log bounded while retaining independent versions and fingerprints for the same key.
    slot.pending.retain(|record| !record.same_slot(&pending));
    slot.pending.push(pending);
    ensure_flush_at_exit();
}

/// One process-wide cache. The archive is decompressed once; later lookups copy one payload.
/// Stores stay in memory until the process exits, which writes each pending archive once.
fn dump_cache() -> &'static Mutex<HashMap<PathBuf, CacheSlot>> {
    static CACHE: std::sync::OnceLock<Mutex<HashMap<PathBuf, CacheSlot>>> =
        std::sync::OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

struct CacheSlot {
    /// Stamp of the file this memory was reconciled with. `None` when that file does not exist yet.
    stamp: Option<Stamp>,
    archive: Archive,
    /// Exact publications not yet written. Replaying operations, rather than copying a stale final
    /// entry vector, preserves versions concurrently published by another process.
    pending: Vec<PendingDump>,
}

impl CacheSlot {
    fn empty() -> Self {
        Self {
            stamp: None,
            archive: Archive::empty(),
            pending: Vec::new(),
        }
    }
}

#[derive(Clone, Debug)]
struct PendingDump {
    module: String,
    key: String,
    compiler: DumpVersion,
    fingerprint: u128,
    blob: u128,
}

impl PendingDump {
    fn same_slot(&self, other: &Self) -> bool {
        self.module == other.module
            && self.key == other.key
            && self.compiler == other.compiler
            && self.fingerprint == other.fingerprint
    }
}

/// One observed file generation. Modification time and length catch ordinary writes; the Unix
/// identity and change time distinguish an atomic same-size replacement even when its mtime is
/// preserved. This harness already relies on Unix `flock`, so the same platform boundary owns the
/// generation fields.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Stamp {
    modified_seconds: u64,
    modified_nanos: u32,
    len: u64,
    device: u64,
    inode: u64,
    change_seconds: i64,
    change_nanos: i64,
}

struct Archive {
    modules: BTreeMap<String, BTreeMap<String, Vec<RecordedEntry>>>,
    /// Uncompressed archive. Blob ranges point into this buffer.
    body: Vec<u8>,
    blobs: BTreeMap<u128, std::ops::Range<usize>>,
}

impl Archive {
    fn empty() -> Self {
        Self::from_parts(BTreeMap::new(), BTreeMap::new())
    }

    fn blob(&self, id: u128) -> Option<&[u8]> {
        let range = self.blobs.get(&id)?;
        self.body.get(range.clone())
    }

    fn owned_blobs(&self) -> BTreeMap<u128, Vec<u8>> {
        self.blobs
            .iter()
            .map(|(id, range)| (*id, self.body[range.clone()].to_vec()))
            .collect()
    }

    fn insert_blob(&mut self, id: u128, raw: Vec<u8>) {
        let mut owned = BTreeMap::new();
        for (existing, range) in &self.blobs {
            if *existing != id {
                owned.insert(*existing, self.body[range.clone()].to_vec());
            }
        }
        owned.insert(id, raw);
        self.retain_referenced(&mut owned);
        *self = Self::from_parts(std::mem::take(&mut self.modules), owned);
    }

    fn retain_referenced(&self, blobs: &mut BTreeMap<u128, Vec<u8>>) {
        let mut referenced = std::collections::BTreeSet::new();
        for entries in self.modules.values() {
            for recordings in entries.values() {
                for recording in recordings {
                    referenced.insert(recording.blob);
                }
            }
        }
        blobs.retain(|id, _| referenced.contains(id));
    }

    fn from_parts(
        modules: BTreeMap<String, BTreeMap<String, Vec<RecordedEntry>>>,
        blobs: BTreeMap<u128, Vec<u8>>,
    ) -> Self {
        Self::try_parse(serialize_archive(&modules, &blobs)).expect("serialized class-dump archive")
    }

    fn try_parse(body: Vec<u8>) -> Option<Self> {
        let split = body.iter().position(|byte| *byte == 0)?;
        let text = std::str::from_utf8(&body[..split]).ok()?;
        let modules = parse_modules(text)?;
        let mut offset = split + 1;
        let count = read_u32(&body, &mut offset)? as usize;
        let mut blobs = BTreeMap::new();
        for _ in 0..count {
            let id_bytes = read_bytes(&body, &mut offset, 16)?;
            let id = u128::from_be_bytes(id_bytes.try_into().ok()?);
            let len = read_u32(&body, &mut offset)? as usize;
            let start = offset;
            let _ = read_bytes(&body, &mut offset, len)?;
            blobs.insert(id, start..offset);
        }
        if offset != body.len() {
            return None;
        }
        if modules.values().any(|entries| {
            entries
                .values()
                .flatten()
                .any(|recording| !blobs.contains_key(&recording.blob))
        }) {
            return None;
        }
        Some(Self {
            modules,
            body,
            blobs,
        })
    }
}

fn serialize_archive(
    modules: &BTreeMap<String, BTreeMap<String, Vec<RecordedEntry>>>,
    blobs: &BTreeMap<u128, Vec<u8>>,
) -> Vec<u8> {
    let mut body = render_modules(modules).into_bytes();
    body.push(0);
    body.extend_from_slice(&(blobs.len() as u32).to_be_bytes());
    for (id, raw) in blobs {
        body.extend_from_slice(&id.to_be_bytes());
        body.extend_from_slice(&(raw.len() as u32).to_be_bytes());
        body.extend_from_slice(raw);
    }
    body
}

fn render_modules(modules: &BTreeMap<String, BTreeMap<String, Vec<RecordedEntry>>>) -> String {
    let mut text = String::from(
        "# kotlinc outputs keyed by exact compiler version. RC tags use a separate slot.\n\
         # A snapshot compiler never uses this file.\n",
    );
    for (module, entries) in modules {
        text.push_str(&format!("\n[[{module}]]\n"));
        for (key, recordings) in entries {
            text.push_str(&format!("[{key}]\n"));
            let mut ordered = recordings.clone();
            ordered.sort_by_key(|entry| {
                (
                    entry.channel == Channel::Rc,
                    entry.version,
                    entry.fingerprint,
                )
            });
            for entry in ordered {
                text.push_str(&render_entry(entry));
                text.push('\n');
            }
        }
    }
    text
}

fn parse_modules(text: &str) -> Option<BTreeMap<String, BTreeMap<String, Vec<RecordedEntry>>>> {
    let mut modules: BTreeMap<String, BTreeMap<String, Vec<RecordedEntry>>> = BTreeMap::new();
    let mut module: Option<String> = None;
    let mut key: Option<String> = None;
    for line in text.lines() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(name) = line
            .strip_prefix("[[")
            .and_then(|line| line.strip_suffix("]]"))
        {
            modules.entry(name.to_string()).or_default();
            module = Some(name.to_string());
            key = None;
            continue;
        }
        if let Some(name) = line
            .strip_prefix('[')
            .and_then(|line| line.strip_suffix(']'))
        {
            let entries = module.as_ref().and_then(|module| modules.get_mut(module))?;
            entries.entry(name.to_string()).or_default();
            key = Some(name.to_string());
            continue;
        }
        let mut parts = line.split_whitespace();
        let version = parts.next()?;
        let fingerprint = parse_hex128(parts.next()?)?;
        let blob = parse_hex128(parts.next()?)?;
        if parts.next().is_some() {
            return None;
        }
        let parsed = if version.contains("..") {
            legacy_range_entries(version, fingerprint, blob)?
        } else {
            let (channel, version) = parse_bound(version)?;
            vec![RecordedEntry {
                version,
                channel,
                fingerprint,
                blob,
            }]
        };
        let recordings = module
            .as_ref()
            .zip(key.as_ref())
            .and_then(|(module, key)| modules.get_mut(module)?.get_mut(key))?;
        recordings.extend(parsed);
    }
    Some(modules)
}

/// `2.4.20..` is the release that was current when the line was written. A closed range
/// (`2.4.0..2.4.10`) names only the supported releases inside it. Neither form extends to a
/// compiler the recording did not name.
fn legacy_range_entries(token: &str, fingerprint: u128, blob: u128) -> Option<Vec<RecordedEntry>> {
    let (lo_token, hi_token) = token.split_once("..")?;
    let (channel, lo) = parse_bound(lo_token)?;
    let versions = if hi_token.is_empty() {
        vec![lo]
    } else {
        let (hi_channel, hi) = parse_bound(hi_token)?;
        if hi_channel != channel || hi < lo {
            return None;
        }
        KotlinVersion::supported()
            .into_iter()
            .filter(|version| *version >= lo && *version <= hi)
            .collect()
    };
    Some(
        versions
            .into_iter()
            .map(|version| RecordedEntry {
                version,
                channel,
                fingerprint,
                blob,
            })
            .collect(),
    )
}

fn cached_archive<'a>(
    path: &Path,
    cache: &'a mut HashMap<PathBuf, CacheSlot>,
) -> Option<&'a Archive> {
    reconcile(path, cache);
    let slot = cache.get(path)?;
    if slot.stamp.is_none() && slot.pending.is_empty() {
        return None;
    }
    Some(&slot.archive)
}

/// Pull a file written by another process into memory without dropping dumps this process has not
/// published yet.
fn reconcile(path: &Path, cache: &mut HashMap<PathBuf, CacheSlot>) {
    reconcile_with(path, cache, ReconcileMode::Observe);
}

/// Reconcile while the publication lock is held. A dirty slot must read the archive even when its
/// metadata stamp appears unchanged: another process can atomically publish a same-size file with
/// the same coarse timestamp. Replaying over that file is what prevents a lost publication.
fn reconcile_for_publication(path: &Path, cache: &mut HashMap<PathBuf, CacheSlot>) {
    reconcile_with(path, cache, ReconcileMode::LockedPublication);
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ReconcileMode {
    Observe,
    LockedPublication,
}

fn reconcile_with(path: &Path, cache: &mut HashMap<PathBuf, CacheSlot>, mode: ReconcileMode) {
    let on_disk = file_stamp(path);
    let Some(slot) = cache.remove(path) else {
        if let Some(stamp) = on_disk {
            if let Some(archive) = read_archive(path) {
                cache.insert(
                    path.to_path_buf(),
                    CacheSlot {
                        stamp: Some(stamp),
                        archive,
                        pending: Vec::new(),
                    },
                );
            }
        }
        return;
    };
    let reload_pending = mode == ReconcileMode::LockedPublication && !slot.pending.is_empty();
    if slot.stamp == on_disk && !reload_pending {
        cache.insert(path.to_path_buf(), slot);
        return;
    }
    if slot.pending.is_empty() {
        if let Some(stamp) = on_disk {
            if let Some(archive) = read_archive(path) {
                cache.insert(
                    path.to_path_buf(),
                    CacheSlot {
                        stamp: Some(stamp),
                        archive,
                        pending: Vec::new(),
                    },
                );
            }
        }
        // A clean slot never outlives deletion, replacement with an unreadable archive, or
        // corruption. Serving its old memory would make a removed cache entry appear valid.
        return;
    }

    let (stamp, mut disk) = match (slot.stamp, on_disk) {
        // A newly created in-memory archive has no disk predecessor yet.
        (None, None) => {
            cache.insert(path.to_path_buf(), slot);
            return;
        }
        (_, Some(stamp)) => {
            let Some(archive) = read_archive(path) else {
                // The file is not a recording this process can merge. Publication holds the
                // directory lock, so this is not a partial rename. Keep the in-memory recordings;
                // the caller replaces the file instead of leaving every later lookup to miss.
                if !slot.pending.is_empty() {
                    cache.insert(path.to_path_buf(), slot);
                }
                return;
            };
            (Some(stamp), archive)
        }
        // A previously observed archive was deleted. Respect deletion instead of recreating it
        // from stale process memory.
        (Some(_), None) => return,
    };
    replay_pending(&mut disk, &slot.archive, &slot.pending);
    cache.insert(
        path.to_path_buf(),
        CacheSlot {
            stamp,
            archive: disk,
            pending: slot.pending,
        },
    );
}

fn replay_pending(into: &mut Archive, from: &Archive, pending: &[PendingDump]) {
    let mut blobs = into.owned_blobs();
    for record in pending {
        let Some(raw) = from.blob(record.blob) else {
            // A later coalesced publication superseded this blob in the in-memory archive.
            continue;
        };
        blobs.insert(record.blob, raw.to_vec());
        let mut recordings = into
            .modules
            .get(&record.module)
            .and_then(|entries| entries.get(&record.key))
            .cloned()
            .unwrap_or_default();
        recordings.retain(|entry| {
            entry.channel != record.compiler.channel
                || entry.version != record.compiler.version
                || entry.fingerprint != record.fingerprint
        });
        recordings.push(RecordedEntry {
            version: record.compiler.version,
            channel: record.compiler.channel,
            fingerprint: record.fingerprint,
            blob: record.blob,
        });
        recordings.sort_by_key(|entry| {
            (
                entry.channel == Channel::Rc,
                entry.version,
                entry.fingerprint,
            )
        });
        into.modules
            .entry(record.module.clone())
            .or_default()
            .insert(record.key.clone(), recordings);
    }
    let mut referenced = BTreeSet::new();
    for entries in into.modules.values() {
        for recordings in entries.values() {
            for recording in recordings {
                referenced.insert(recording.blob);
            }
        }
    }
    blobs.retain(|id, _| referenced.contains(id));
    let modules = std::mem::take(&mut into.modules);
    *into = Archive::from_parts(modules, blobs);
}

fn read_archive(path: &Path) -> Option<Archive> {
    let bytes = std::fs::read(path).ok()?;
    Archive::try_parse(try_decompress(&bytes)?)
}

/// Write every archive this process has changed. A directory that has already been removed is
/// skipped, so a test that deletes its scratch root does not recreate it.
fn flush_dirty_archives() {
    let paths: Vec<PathBuf> = {
        let cache = dump_cache().lock().expect("class-dump cache");
        cache
            .iter()
            .filter(|(_, slot)| !slot.pending.is_empty())
            .map(|(path, _)| path.clone())
            .collect()
    };
    for path in paths {
        if let Some(root) = path.parent() {
            flush_archive(root);
        }
    }
}

fn flush_archive(root: &Path) {
    let path = archive_path(root);
    let Some(parent) = path.parent() else {
        return;
    };
    let mut cache = dump_cache().lock().expect("class-dump cache");
    if cache.get(&path).is_none_or(|slot| slot.pending.is_empty()) {
        return;
    }
    if !parent.exists() {
        cache.remove(&path);
        return;
    }
    let _lock = lock_directory(parent);
    reconcile_for_publication(&path, &mut cache);
    let Some(slot) = cache.get(&path) else {
        return;
    };
    if slot.pending.is_empty() {
        return;
    }
    let bytes = compress(&slot.archive.body);
    write_atomic(&path, &bytes);
    let stamp = file_stamp(&path).expect("written class-dump archive");
    let slot = cache.get_mut(&path).expect("class-dump slot");
    slot.stamp = Some(stamp);
    slot.pending.clear();
}

fn ensure_flush_at_exit() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| unsafe {
        let registered = libc::atexit(flush_at_exit);
        assert_eq!(registered, 0, "register class-dump exit flush");
    });
}

extern "C" fn flush_at_exit() {
    // No panic may cross the C ABI boundary. Normal test-time explicit flushes retain their exact
    // diagnostics; process teardown is best-effort and must never abort an otherwise valid run.
    let _ = std::panic::catch_unwind(flush_dirty_archives);
}

fn file_stamp(path: &Path) -> Option<Stamp> {
    let meta = std::fs::metadata(path).ok()?;
    let modified = meta.modified().ok()?;
    let since = modified.duration_since(SystemTime::UNIX_EPOCH).ok()?;
    Some(Stamp {
        modified_seconds: since.as_secs(),
        modified_nanos: since.subsec_nanos(),
        len: meta.len(),
        device: meta.dev(),
        inode: meta.ino(),
        change_seconds: meta.ctime(),
        change_nanos: meta.ctime_nsec(),
    })
}

fn archive_path(root: &Path) -> PathBuf {
    root.join("recorded-bytes.zz")
}

fn dumps_root() -> PathBuf {
    if let Some(dir) = std::env::var_os("KRUSTY_CLASS_DUMP_DIR") {
        return PathBuf::from(dir);
    }
    Path::new(env!("CARGO_MANIFEST_DIR")).join("target/cache/class-dumps")
}

fn lock_directory(dir: &Path) -> std::fs::File {
    let file = std::fs::File::open(dir).expect("open class-dump directory");
    // SAFETY: `flock` on a descriptor this function owns until it returns.
    let locked = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
    assert_eq!(locked, 0, "lock class-dump directory");
    file
}

fn write_atomic(path: &Path, bytes: &[u8]) {
    static TEMP: AtomicU64 = AtomicU64::new(0);
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .expect("class-dump archive name");
    let staging = path.with_file_name(format!(
        "{name}.{}.{}",
        std::process::id(),
        TEMP.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::write(&staging, bytes).expect("write class-dump archive");
    std::fs::rename(&staging, path).expect("replace class-dump archive");
}

fn compress(raw: &[u8]) -> Vec<u8> {
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::new(9));
    encoder.write_all(raw).expect("compress class dump");
    encoder.finish().expect("finish class dump")
}

fn decompress(bytes: &[u8]) -> Vec<u8> {
    try_decompress(bytes).expect("decompress class dump")
}

fn try_decompress(bytes: &[u8]) -> Option<Vec<u8>> {
    let mut raw = Vec::new();
    ZlibDecoder::new(bytes).read_to_end(&mut raw).ok()?;
    Some(raw)
}

fn encode_raw(files: &BTreeMap<String, Vec<u8>>) -> Vec<u8> {
    let mut raw = Vec::new();
    for (name, bytes) in files {
        raw.extend_from_slice(&(name.len() as u32).to_be_bytes());
        raw.extend_from_slice(name.as_bytes());
        raw.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
        raw.extend_from_slice(bytes);
    }
    raw
}

fn decode_raw(raw: &[u8]) -> Option<BTreeMap<String, Vec<u8>>> {
    let mut files = BTreeMap::new();
    let mut offset = 0;
    while offset < raw.len() {
        let name_len = read_u32(raw, &mut offset)? as usize;
        let name = std::str::from_utf8(read_bytes(raw, &mut offset, name_len)?).ok()?;
        let data_len = read_u32(raw, &mut offset)? as usize;
        let data = read_bytes(raw, &mut offset, data_len)?.to_vec();
        files.insert(name.to_string(), data);
    }
    Some(files)
}

fn read_u32(raw: &[u8], offset: &mut usize) -> Option<u32> {
    let bytes = read_bytes(raw, offset, 4)?;
    Some(u32::from_be_bytes(bytes.try_into().ok()?))
}

fn read_bytes<'a>(raw: &'a [u8], offset: &mut usize, len: usize) -> Option<&'a [u8]> {
    let end = offset.checked_add(len)?;
    let bytes = raw.get(*offset..end)?;
    *offset = end;
    Some(bytes)
}

fn parse_bound(token: &str) -> Option<(Channel, KotlinVersion)> {
    match token.strip_suffix("-RC") {
        Some(version) => Some((Channel::Rc, KotlinVersion::parse(version)?)),
        None => Some((Channel::Release, KotlinVersion::parse(token)?)),
    }
}

fn parse_hex128(text: &str) -> Option<u128> {
    u128::from_str_radix(text, 16).ok()
}

fn render_entry(entry: RecordedEntry) -> String {
    let version = match entry.channel {
        Channel::Release => entry.version.to_string(),
        Channel::Rc => format!("{}-RC", entry.version),
    };
    format!(
        "{version} {} {}",
        hex128(entry.fingerprint),
        hex128(entry.blob)
    )
}

fn hex128(value: u128) -> String {
    format!("{value:032x}")
}

fn sanitize(component: &str) -> String {
    let mut text: String = component
        .chars()
        .map(|ch| match ch {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '.' | '_' | '-' | '$' => ch,
            _ => '_',
        })
        .collect();
    if text.is_empty() || text == "." || text == ".." {
        text = format!("_{:016x}", fnv64(0xcbf29ce484222325, component.as_bytes()));
    }
    if text.len() > 120 {
        let hash = fnv64(0xcbf29ce484222325, component.as_bytes());
        text.truncate(100);
        text.push_str(&format!("_{hash:016x}"));
    }
    text
}

fn record_forced() -> bool {
    ["KRUSTY_RECORD", "KRUSTY_RECORD_CLASS_DUMPS"]
        .iter()
        .any(|name| std::env::var_os(name).is_some_and(|flag| flag == "1"))
}

fn compile_missing_allowed() -> bool {
    std::env::var_os("KRUSTY_CLASS_DUMP_COMPILE_MISSING").is_some_and(|flag| flag == "1")
}

thread_local! {
    static REQUIRE_DIAGNOSTICS: Cell<bool> = const { Cell::new(false) };
}

/// Run `body` as an assert against kotlinc's exit code and diagnostics. A release dump that has
/// class files but no recorded status fails instead of compiling or inventing a successful run.
pub fn with_recorded_diagnostics<T>(body: impl FnOnce() -> T) -> T {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            REQUIRE_DIAGNOSTICS.with(|flag| flag.set(self.0));
        }
    }
    let previous = REQUIRE_DIAGNOSTICS.with(|flag| flag.replace(true));
    let _restore = Restore(previous);
    body()
}

fn diagnostics_required() -> bool {
    REQUIRE_DIAGNOSTICS.with(Cell::get)
}

const INVOCATION_MODULE: &str = "_invocations";
const JAR_ENTRY: &str = "__jar__";
const EXIT_ENTRY: &str = "__exit__";
const STDERR_ENTRY: &str = "__stderr__";

pub(crate) struct ReplayedClasses {
    pub(crate) code: i32,
    pub(crate) stderr: String,
    /// `false` for a class dump recorded before exit codes and diagnostics were stored.
    pub(crate) status: bool,
    pub(crate) files: BTreeMap<String, Vec<u8>>,
}

struct Invocation {
    out: PathBuf,
    fingerprint: u128,
    legacy_fingerprint: u128,
    label: String,
}

/// Class files, exit code, and diagnostics recorded for this `kotlinc` invocation.
///
/// `Some` must not compile. `None` means the caller compiles: recording is forced, CI explicitly
/// permits a read-only compile for a missing entry, or a compiler does not use the archive. A
/// release or RC with no matching dump otherwise panics. Inside [`with_recorded_diagnostics`], a
/// dump that has class files but no exit code or diagnostics likewise panics unless CI may compile
/// that entry live: an assert must not treat the incomplete recording as a successful empty report.
pub fn replay_class_dump(args: &[String]) -> Option<ReplayedClasses> {
    replay_class_dump_with_policy(
        args,
        &dumps_root(),
        compiler_dump_version(),
        record_forced(),
        compile_missing_allowed(),
    )
}

fn replay_class_dump_with_policy(
    args: &[String],
    root: &Path,
    compiler: Option<DumpVersion>,
    force: bool,
    compile_missing: bool,
) -> Option<ReplayedClasses> {
    let compiler = compiler?;
    if force {
        return None;
    }
    let invocation = match parse_invocation(args) {
        Ok(Some(invocation)) => invocation,
        Ok(None) => refuse_missing_dump(
            "a kotlinc invocation that does not name an output directory and source files",
        ),
        Err(err) => refuse_missing_dump(&err),
    };
    let mut saw_incomplete = false;
    for fingerprint in [invocation.fingerprint, invocation.legacy_fingerprint] {
        let key = hex128(fingerprint);
        let Some(files) = load_files(root, INVOCATION_MODULE, &key, compiler, fingerprint) else {
            if fingerprint == invocation.legacy_fingerprint {
                break;
            }
            continue;
        };
        let replayed = split_replay(files);
        if diagnostics_required() && !replayed.status {
            saw_incomplete = true;
            if fingerprint == invocation.legacy_fingerprint {
                break;
            }
            continue;
        }
        return Some(replayed);
    }
    if saw_incomplete && !compile_missing {
        refuse_missing_dump(&format!(
            "sources {} fingerprint {} (class files are recorded, but not the exit code and diagnostics)",
            invocation.label,
            hex128(invocation.fingerprint)
        ));
    }
    if compile_missing || saw_incomplete {
        return None;
    }
    refuse_missing_dump(&format!(
        "sources {} fingerprint {}",
        invocation.label,
        hex128(invocation.fingerprint)
    ));
}

/// Write a replayed dump into the invocation's `-d` path.
pub fn write_replayed_classes(args: &[String], files: &BTreeMap<String, Vec<u8>>) {
    let invocation = match parse_invocation(args) {
        Ok(Some(invocation)) => invocation,
        Ok(None) | Err(_) => panic!("replayed class dump names an output directory"),
    };
    write_output(&invocation.out, files);
}

/// Store the class files, exit code, and diagnostics from a live compile.
///
/// A forced re-record stores every compile. A cache miss stores only the invocation that
/// actually compiled, and only when this run publishes the archive. A pull request compiles
/// the miss and leaves the restored archive unchanged. A non-release compiler does not write.
pub fn remember_class_dump(args: &[String], code: i32, stderr: &str) {
    remember_live_compile(
        args,
        code,
        stderr,
        &dumps_root(),
        compiler_dump_version(),
        ci_allows_write() && (record_forced() || compile_missing_allowed()),
    );
}

fn remember_live_compile(
    args: &[String],
    code: i32,
    stderr: &str,
    root: &Path,
    compiler: Option<DumpVersion>,
    store: bool,
) {
    if !store {
        return;
    }
    let Some(compiler) = compiler else {
        return;
    };
    let invocation = match parse_invocation(args) {
        Ok(Some(invocation)) => invocation,
        Ok(None) => return,
        Err(err) => panic!("{err}"),
    };
    let mut files = read_output_tree(&invocation.out).unwrap_or_default();
    attach_status(&mut files, code, stderr);
    store_files(
        root,
        INVOCATION_MODULE,
        &hex128(invocation.fingerprint),
        compiler,
        invocation.fingerprint,
        &files,
    );
}

fn attach_status(files: &mut BTreeMap<String, Vec<u8>>, code: i32, stderr: &str) {
    files.insert(EXIT_ENTRY.to_string(), code.to_le_bytes().to_vec());
    files.insert(STDERR_ENTRY.to_string(), stderr.as_bytes().to_vec());
}

fn split_replay(mut files: BTreeMap<String, Vec<u8>>) -> ReplayedClasses {
    let status = files.contains_key(EXIT_ENTRY);
    let code = match files.remove(EXIT_ENTRY) {
        Some(bytes) => exit_code(&bytes),
        None => 0,
    };
    let stderr = files
        .remove(STDERR_ENTRY)
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .unwrap_or_default();
    ReplayedClasses {
        code,
        stderr,
        status,
        files,
    }
}

fn exit_code(bytes: &[u8]) -> i32 {
    let Ok(bytes) = <[u8; 4]>::try_from(bytes) else {
        refuse_missing_dump("a kotlinc exit code that is not four bytes");
    };
    i32::from_le_bytes(bytes)
}

fn parse_invocation(args: &[String]) -> Result<Option<Invocation>, String> {
    let mut out = None;
    let mut sources: Vec<(String, Vec<u8>)> = Vec::new();
    let mut classpath = Vec::new();
    let mut flags = Vec::new();
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        if arg == "-d" || arg == "-destination" {
            out = args.get(index + 1).map(PathBuf::from);
            index += 2;
            continue;
        }
        if arg == "-cp" || arg == "-classpath" {
            if let Some(value) = args.get(index + 1) {
                classpath.extend(std::env::split_paths(value));
            }
            index += 2;
            continue;
        }
        if let Some(value) = arg
            .strip_prefix("-cp=")
            .or_else(|| arg.strip_prefix("-classpath="))
        {
            classpath.extend(std::env::split_paths(value));
            index += 1;
            continue;
        }
        if is_source_arg(arg) {
            let bytes = std::fs::read(arg)
                .map_err(|err| format!("unreadable source {}: {err}", Path::new(arg).display()))?;
            let name = basename(arg);
            sources.push((name, bytes));
            index += 1;
            continue;
        }
        flags.push(normalize_invocation_flag(arg)?);
        index += 1;
    }
    let Some(out) = out else {
        return Ok(None);
    };
    if sources.is_empty() {
        return Ok(None);
    }
    let label = sources
        .iter()
        .map(|(name, _)| name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let mut blob = Vec::new();
    for (name, bytes) in &sources {
        blob.extend_from_slice(name.as_bytes());
        blob.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
        blob.extend_from_slice(bytes);
    }
    let legacy_classpath = legacy_classpath_fingerprint(&classpath)?;
    let classpath = classpath_content_fingerprint(&classpath)?;
    let flags = flags.join("\n");
    let fingerprint_of = |classpath: &str| {
        fingerprint_parts(&[blob.as_slice(), flags.as_bytes(), classpath.as_bytes()])
    };
    Ok(Some(Invocation {
        out,
        fingerprint: fingerprint_of(&classpath),
        legacy_fingerprint: fingerprint_of(&legacy_classpath),
        label,
    }))
}

fn is_source_arg(arg: &str) -> bool {
    let path = Path::new(arg);
    let ext = path.extension().and_then(|ext| ext.to_str());
    matches!(ext, Some("kt" | "kts" | "java")) && path.is_file()
}

fn read_output_tree(out: &Path) -> Option<BTreeMap<String, Vec<u8>>> {
    let mut files = BTreeMap::new();
    if out.is_file() {
        files.insert(JAR_ENTRY.to_string(), std::fs::read(out).ok()?);
        return Some(files);
    }
    if out.exists() {
        read_tree(out, out, &mut files)?;
    }
    Some(files)
}

fn read_tree(root: &Path, dir: &Path, files: &mut BTreeMap<String, Vec<u8>>) -> Option<()> {
    for entry in std::fs::read_dir(dir).ok()? {
        let path = entry.ok()?.path();
        if path.is_dir() {
            read_tree(root, &path, files)?;
        } else if path.is_file() {
            let relative = path.strip_prefix(root).ok()?;
            let name = relative.to_string_lossy().replace('\\', "/");
            files.insert(name, std::fs::read(&path).ok()?);
        }
    }
    Some(())
}

fn write_output(out: &Path, files: &BTreeMap<String, Vec<u8>>) {
    if let Some(bytes) = files.get(JAR_ENTRY) {
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent).expect("create kotlinc output directory");
        }
        std::fs::write(out, bytes).expect("write replayed kotlinc jar");
        return;
    }
    std::fs::create_dir_all(out).expect("create kotlinc output directory");
    for (relative, bytes) in files {
        if relative == EXIT_ENTRY || relative == STDERR_ENTRY {
            continue;
        }
        let path = out.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create replayed class directory");
        }
        std::fs::write(path, bytes).expect("write replayed class file");
    }
}

fn ci_allows_write() -> bool {
    std::env::var_os("CI").is_none()
        || std::env::var_os("KRUSTY_CLASS_DUMP_WRITE").is_some_and(|flag| flag == "1")
}

/// Why `kotlinc_compile` ran the real compiler instead of replaying a dump.
fn classify_live_kotlinc(cached_compiler: bool, force: bool) -> &'static str {
    if !cached_compiler {
        "uncached-compiler"
    } else if force {
        "record"
    } else {
        "cache-miss"
    }
}

/// One greppable stderr line for a live kotlinc invocation.
///
/// libtest captures `eprintln!` and hides it when the test passes, so the line is written to file
/// descriptor 2 directly. A process-exit summary repeats each test's count.
fn format_live_kotlinc_line(reason: &str, test: &str, sources: &str, fingerprint: &str) -> String {
    format!(
        "class-dump: live kotlinc {reason} test={test} sources={sources} fingerprint={fingerprint}\n"
    )
}

struct LiveKotlincUse {
    reason: &'static str,
    test: String,
}

struct LiveKotlincLog {
    uses: Mutex<Vec<LiveKotlincUse>>,
}

fn summarize_live_kotlinc(uses: &[LiveKotlincUse]) -> String {
    let mut counts: BTreeMap<(String, &'static str), usize> = BTreeMap::new();
    for use_ in uses {
        *counts.entry((use_.test.clone(), use_.reason)).or_default() += 1;
    }
    let mut summary = format!(
        "class-dump: live kotlinc summary invocations={} tests={}\n",
        uses.len(),
        counts.len()
    );
    for ((test, reason), count) in counts {
        summary.push_str(&format!(
            "class-dump: live kotlinc summary {reason} test={test} invocations={count}\n"
        ));
    }
    summary
}

impl Drop for LiveKotlincLog {
    fn drop(&mut self) {
        let uses = std::mem::take(self.uses.get_mut().unwrap_or_else(|err| err.into_inner()));
        if !uses.is_empty() {
            write_process_stderr(&summarize_live_kotlinc(&uses));
        }
    }
}

/// libtest ends the process with `exit`, which skips Rust destructors. This still runs.
extern "C" fn print_live_kotlinc_summary() {
    let Some(log) = LIVE_KOTLINC_LOG.get() else {
        return;
    };
    let uses = std::mem::take(&mut *log.uses.lock().unwrap_or_else(|err| err.into_inner()));
    if !uses.is_empty() {
        write_process_stderr(&summarize_live_kotlinc(&uses));
    }
}

fn install_live_kotlinc_summary() {
    static HOOK: std::sync::Once = std::sync::Once::new();
    HOOK.call_once(|| {
        // SAFETY: the handler is `extern "C"`, reads only this process's log, and runs when the
        // process is exiting, after test threads have finished.
        unsafe {
            libc::atexit(print_live_kotlinc_summary);
        }
    });
}

static LIVE_KOTLINC_LOG: std::sync::OnceLock<LiveKotlincLog> = std::sync::OnceLock::new();

fn live_kotlinc_log() -> &'static LiveKotlincLog {
    LIVE_KOTLINC_LOG.get_or_init(|| LiveKotlincLog {
        uses: Mutex::new(Vec::new()),
    })
}

fn write_process_stderr(text: &str) {
    let bytes = text.as_bytes();
    let mut offset = 0;
    while offset < bytes.len() {
        // SAFETY: `bytes` is live for this call, and fd 2 is the process stderr.
        let written =
            unsafe { libc::write(2, bytes[offset..].as_ptr().cast(), bytes.len() - offset) };
        if written < 0 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return;
        }
        if written == 0 {
            return;
        }
        offset += written as usize;
    }
}

/// Report that this test just ran kotlinc instead of replaying recorded class files.
pub(crate) fn report_live_kotlinc(args: &[String]) {
    let reason = classify_live_kotlinc(compiler_dump_version().is_some(), record_forced());
    let test = std::thread::current()
        .name()
        .unwrap_or("unknown")
        .to_string();
    let (sources, fingerprint) = match parse_invocation(args) {
        Ok(Some(invocation)) => (invocation.label, hex128(invocation.fingerprint)),
        Ok(None) => ("(no sources)".to_string(), "-".to_string()),
        Err(_) => ("(unreadable sources)".to_string(), "-".to_string()),
    };
    let line = format_live_kotlinc_line(reason, &test, &sources, &fingerprint);
    write_process_stderr(&line);
    install_live_kotlinc_summary();
    live_kotlinc_log()
        .uses
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .push(LiveKotlincUse { reason, test });
}

fn running_test() -> (String, String) {
    let thread = std::thread::current();
    let name = thread.name().unwrap_or_default();
    match name.split_once("::") {
        Some((module, case)) => (module.to_string(), case.to_string()),
        None => panic!(
            "class dumps are keyed by the running test's path, but this thread is named \
             {name:?}; call them from the test's own thread"
        ),
    }
}

#[cfg(test)]
mod tests;
