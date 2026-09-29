//! Kotlinc class-file dumps for byte-equality checks.
//!
//! A check compares krusty against a dump recorded from kotlinc for this fixture, and runs kotlinc
//! only when the dump is missing or its fingerprint no longer matches the fixture. A release
//! (`2.4.20`, `2.4.20-release-482`) and an RC tag (`2.4.20-RC`, `2.4.20-RC2`) may read and write
//! dumps. A snapshot, dev, or beta build never does: that version string is not an immutable
//! artifact.
//!
//! What is stored is an open version range, not a copy per compiler build. `2.4.20..` covers that
//! release and every newer one until a later recording disagrees, so adding a Kotlin version does
//! not rewrite the dumps. RC tags of one release share `2.4.20-RC..` and do not share the release
//! range. The bytes live in one zlib archive, `tests/recorded-bytes.zz`. Identical outputs are
//! stored once inside it, and the whole archive compresses together. A text index in that archive
//! records the open range for each dump.
//!
//! `KRUSTY_RECORD=1` ignores a stored dump, recompiles, and rewrites the archive. A release or RC
//! with no matching dump fails the test instead of compiling: the archive has to be updated and
//! committed. A rejected kotlinc run is recorded too, exit code and stderr included, and a later
//! run replays that rejection. A snapshot, dev, or beta build still compiles, because that version
//! is not an immutable artifact and never reads or writes the archive. CI does not write dumps.
//!
//! The archive is read at runtime and is not compiled into the test binary.

use std::cell::Cell;
use std::collections::{BTreeMap, HashMap};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
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
/// The value is `build.txt`'s first line, unchanged. Dump lookup then folds that identity into an
/// open version range: a release build and `release-N` of the same version share one range, and
/// RC tags of that version share a separate one.
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

/// Inputs that decide which dump a fixture uses, with no absolute paths in either field.
///
/// Jar classpath entries contribute their file name (the compiler build id already pins the dist
/// jars). A directory entry contributes a content hash of the files under it and not the
/// directory's own name, so a scratch classpath invalidates the dump when its bytes change and
/// still hits on the next run. `variant` is the kotlinc arguments with path values reduced to
/// file names.
pub struct ClassDumpInputs {
    pub fingerprint: u128,
    pub variant: String,
}

pub fn class_dump_inputs(
    source: &str,
    jvm_target: &str,
    extra_args: &[String],
    classpath: &[PathBuf],
) -> ClassDumpInputs {
    let variant = normalize_args(extra_args);
    let classpath = classpath_fingerprint(classpath);
    ClassDumpInputs {
        fingerprint: fingerprint_parts(&[
            source.as_bytes(),
            jvm_target.as_bytes(),
            variant.as_bytes(),
            classpath.as_bytes(),
        ]),
        variant,
    }
}

fn normalize_args(args: &[String]) -> String {
    args.iter()
        .map(|arg| match arg.split_once('=') {
            Some((flag, value)) => format!("{flag}={}", basename(value)),
            None => basename(arg),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn basename(value: &str) -> String {
    std::path::Path::new(value)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(value)
        .to_string()
}

fn classpath_fingerprint(paths: &[PathBuf]) -> String {
    let mut rows: Vec<String> = paths
        .iter()
        .map(|path| {
            if path.is_dir() {
                format!("dir:{:016x}", hash_tree(path))
            } else {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("")
                    .to_string()
            }
        })
        .collect();
    rows.sort();
    rows.join("\n")
}

fn hash_tree(root: &Path) -> u64 {
    let mut files = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.is_file() {
                files.push(path);
            }
        }
    }
    files.sort();
    let mut hash = 0xcbf29ce484222325u64;
    for path in files {
        if let Ok(relative) = path.strip_prefix(root) {
            hash = fnv64(
                hash,
                relative.to_string_lossy().replace('\\', "/").as_bytes(),
            );
        }
        if let Ok(bytes) = std::fs::read(&path) {
            hash = fnv64(hash, &bytes);
        }
    }
    hash
}

pub(crate) fn fingerprint_parts(parts: &[&[u8]]) -> u128 {
    let mut high = 0xcbf29ce484222325u64;
    let mut low = 0x84222325cbf29ce4u64;
    for part in parts {
        high = fnv64(high, &(part.len() as u64).to_le_bytes());
        high = fnv64(high, part);
        low = fnv64(low, part);
        low = fnv64(low, &(part.len() as u64).to_le_bytes());
    }
    ((high as u128) << 64) | low as u128
}

fn fnv64(mut hash: u64, bytes: &[u8]) -> u64 {
    for byte in bytes {
        hash = (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3);
    }
    hash
}

/// Kotlinc's bytes for each of `classes`, from the recorded dump or from `compile`.
///
/// `compile` runs only for `KRUSTY_RECORD=1`, or for a compiler that is not a release or RC.
/// A release or RC with no matching dump fails the test and does not compile. `None` from
/// `compile` yields `None`. A non-release compiler always takes `compile` and does not touch the
/// archive.
pub fn kotlinc_class_dumps(
    stem: &str,
    jvm_target: &str,
    variant: &str,
    fingerprint: u128,
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
            force: record_forced(),
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
            force: record_forced(),
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
/// `None` when this compiler is not a release or RC, when `KRUSTY_RECORD=1`, or when no open
/// range covers this compiler's version at `fingerprint`.
pub fn load_shared_files(slot: &str, fingerprint: u128) -> Option<BTreeMap<String, Vec<u8>>> {
    if record_forced() {
        return None;
    }
    let compiler = compiler_dump_version()?;
    load_files(&dumps_root(), "_libs", slot, compiler, fingerprint)
}

/// Record `files` (relative path → bytes, including `META-INF` entries) for [`load_shared_files`].
///
/// A no-op for a snapshot, dev, or beta compiler, and under CI. A newer release covered by an open
/// range whose bytes already match does not rewrite the archive.
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
    force: bool,
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
        if let Some(hit) = load_files(
            query.root,
            query.module,
            query.key,
            compiler,
            query.fingerprint,
        )
        .filter(|hit| accept(hit))
        {
            return Some(hit);
        }
        refuse_missing_dump(&format!(
            "module {} key {} fingerprint {:032x}",
            query.module, query.key, query.fingerprint
        ));
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

fn refuse_missing_dump(detail: &str) -> ! {
    let compiler = published_compiler_id().unwrap_or_else(|| "this release".to_string());
    panic!(
        "tests/recorded-bytes.zz has no class dump for {detail} under kotlinc {compiler}. \
         This run does not compile with kotlinc for a release or RC that already uses the archive. \
         Update the archive with KRUSTY_RECORD=1 and commit tests/recorded-bytes.zz."
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
struct Span {
    lo: KotlinVersion,
    hi: Option<KotlinVersion>,
    channel: Channel,
    fingerprint: u128,
    blob: u128,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Recorded {
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
    let spans = archive.modules.get(&sanitize(module))?.get(key)?;
    let span = spans
        .iter()
        .filter(|span| {
            span.channel == compiler.channel
                && span.fingerprint == fingerprint
                && span_contains(span, compiler.version)
        })
        .max_by_key(|span| span.lo)?;
    let raw = archive.blob(span.blob)?.to_vec();
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
    let _lock = lock_directory(parent);
    let mut cache = dump_cache().lock().expect("class-dump cache");
    let stamp = file_stamp(&path);
    let mut archive = take_archive(&path, &mut cache);
    let module = sanitize(module);
    // One test can record two fixtures under one key. Keep each fingerprint's spans; replacing
    // the version slot would drop the earlier fixture and the next run would miss it.
    let current = archive
        .modules
        .get(&module)
        .and_then(|entries| entries.get(key))
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let (mine, rest): (Vec<Span>, Vec<Span>) = current
        .iter()
        .copied()
        .partition(|span| span.channel == compiler.channel && span.fingerprint == fingerprint);
    let mut updated = revised_spans(&mine, compiler, fingerprint, blob);
    updated.extend(rest);
    let unchanged = archive
        .modules
        .get(&module)
        .and_then(|entries| entries.get(key))
        == Some(&updated)
        && archive.blobs.contains_key(&blob);
    if unchanged {
        if let Some(stamp) = stamp {
            cache.insert(path, CacheSlot { stamp, archive });
        }
        return;
    }
    archive
        .modules
        .entry(module)
        .or_default()
        .insert(key.to_string(), updated);
    archive.insert_blob(blob, raw);
    write_atomic(&path, &compress(&archive.body));
    let stamp = file_stamp(&path).expect("written class-dump archive");
    cache.insert(path, CacheSlot { stamp, archive });
}

fn revised_spans(
    current: &[Span],
    compiler: DumpVersion,
    fingerprint: u128,
    blob: u128,
) -> Vec<Span> {
    let mut versions = KotlinVersion::supported();
    if !versions.contains(&compiler.version) {
        versions.push(compiler.version);
        versions.sort();
    }
    let mut projected: Vec<Option<Recorded>> = versions
        .iter()
        .map(|version| {
            current
                .iter()
                .find(|span| span.channel == compiler.channel && span_contains(span, *version))
                .map(|span| Recorded {
                    fingerprint: span.fingerprint,
                    blob: span.blob,
                })
        })
        .collect();
    let position = versions
        .iter()
        .position(|version| *version == compiler.version)
        .expect("the recorded version is in the version list");
    projected[position] = Some(Recorded { fingerprint, blob });
    let updated = merge_spans(&versions, &projected, compiler.channel);
    current
        .iter()
        .copied()
        .filter(|span| span.channel != compiler.channel)
        .chain(updated)
        .collect()
}

fn span_contains(span: &Span, version: KotlinVersion) -> bool {
    span.lo <= version && span.hi.is_none_or(|hi| version <= hi)
}

fn merge_spans(
    versions: &[KotlinVersion],
    values: &[Option<Recorded>],
    channel: Channel,
) -> Vec<Span> {
    let mut spans: Vec<Span> = Vec::new();
    let mut previous: Option<Recorded> = None;
    for (&version, value) in versions.iter().zip(values) {
        match value {
            Some(value) if previous == Some(*value) => {
                spans.last_mut().expect("an open run").hi = Some(version);
            }
            Some(value) => spans.push(Span {
                lo: version,
                hi: Some(version),
                channel,
                fingerprint: value.fingerprint,
                blob: value.blob,
            }),
            None => {}
        }
        previous = *value;
    }
    if let Some(last) = spans.last_mut() {
        if last.hi == versions.last().copied() {
            last.hi = None;
        }
    }
    spans
}

/// One process-wide cache. The archive is decompressed once; later lookups copy one payload.
fn dump_cache() -> &'static Mutex<HashMap<PathBuf, CacheSlot>> {
    static CACHE: std::sync::OnceLock<Mutex<HashMap<PathBuf, CacheSlot>>> =
        std::sync::OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

struct CacheSlot {
    stamp: Stamp,
    archive: Archive,
}

type Stamp = (u64, u32, u64);

struct Archive {
    modules: BTreeMap<String, BTreeMap<String, Vec<Span>>>,
    /// Uncompressed archive. Blob ranges point into this buffer.
    body: Vec<u8>,
    blobs: BTreeMap<u128, std::ops::Range<usize>>,
}

impl Archive {
    fn blob(&self, id: u128) -> Option<&[u8]> {
        let range = self.blobs.get(&id)?;
        self.body.get(range.clone())
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
            for spans in entries.values() {
                for span in spans {
                    referenced.insert(span.blob);
                }
            }
        }
        blobs.retain(|id, _| referenced.contains(id));
    }

    fn from_parts(
        modules: BTreeMap<String, BTreeMap<String, Vec<Span>>>,
        blobs: BTreeMap<u128, Vec<u8>>,
    ) -> Self {
        Self::parse(serialize_archive(&modules, &blobs))
    }

    fn parse(body: Vec<u8>) -> Self {
        let split = body
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or_else(|| panic!("class-dump archive has no blob section"));
        let text = std::str::from_utf8(&body[..split]).expect("class-dump index is utf-8");
        let modules = parse_modules(text);
        let mut offset = split + 1;
        let count = read_u32(&body, &mut offset).expect("class-dump blob count") as usize;
        let mut blobs = BTreeMap::new();
        for _ in 0..count {
            let id_bytes = read_bytes(&body, &mut offset, 16).expect("class-dump blob id");
            let id = u128::from_be_bytes(id_bytes.try_into().expect("16-byte blob id"));
            let len = read_u32(&body, &mut offset).expect("class-dump blob length") as usize;
            let start = offset;
            let _ = read_bytes(&body, &mut offset, len).expect("class-dump blob bytes");
            blobs.insert(id, start..offset);
        }
        if offset != body.len() {
            panic!("trailing bytes in class-dump archive");
        }
        Self {
            modules,
            body,
            blobs,
        }
    }
}

fn serialize_archive(
    modules: &BTreeMap<String, BTreeMap<String, Vec<Span>>>,
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

fn render_modules(modules: &BTreeMap<String, BTreeMap<String, Vec<Span>>>) -> String {
    let mut text = String::from(
        "# kotlinc class dumps. An open range (2.4.20..) covers that release and every newer one.\n\
         # RC tags share a separate range (2.4.20-RC..). A snapshot compiler never uses this file.\n",
    );
    for (module, entries) in modules {
        text.push_str(&format!("\n[[{module}]]\n"));
        for (key, spans) in entries {
            text.push_str(&format!("[{key}]\n"));
            let mut ordered = spans.clone();
            ordered.sort_by_key(|span| (span.channel == Channel::Rc, span.lo));
            for span in ordered {
                text.push_str(&render_span(span));
                text.push('\n');
            }
        }
    }
    text
}

fn parse_modules(text: &str) -> BTreeMap<String, BTreeMap<String, Vec<Span>>> {
    let mut modules: BTreeMap<String, BTreeMap<String, Vec<Span>>> = BTreeMap::new();
    let mut module: Option<String> = None;
    let mut key: Option<String> = None;
    for (index, line) in text.lines().enumerate() {
        let malformed =
            || -> ! { panic!("malformed class-dump index line {}: {line:?}", index + 1) };
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
            match module.as_ref().and_then(|module| modules.get_mut(module)) {
                Some(entries) => {
                    entries.entry(name.to_string()).or_default();
                    key = Some(name.to_string());
                }
                None => malformed(),
            }
            continue;
        }
        let mut parts = line.split_whitespace();
        let range = parts.next().unwrap_or_else(|| malformed());
        let fingerprint = parse_hex128(parts.next().unwrap_or_else(|| malformed()))
            .unwrap_or_else(|| malformed());
        let blob = parse_hex128(parts.next().unwrap_or_else(|| malformed()))
            .unwrap_or_else(|| malformed());
        if parts.next().is_some() {
            malformed();
        }
        let (channel, lo, hi) = parse_range(range).unwrap_or_else(|| malformed());
        match module
            .as_ref()
            .zip(key.as_ref())
            .and_then(|(module, key)| modules.get_mut(module)?.get_mut(key))
        {
            Some(spans) => spans.push(Span {
                lo,
                hi,
                channel,
                fingerprint,
                blob,
            }),
            None => malformed(),
        }
    }
    modules
}

fn cached_archive<'a>(
    path: &Path,
    cache: &'a mut HashMap<PathBuf, CacheSlot>,
) -> Option<&'a Archive> {
    let stamp = file_stamp(path)?;
    let current = cache.get(path).is_some_and(|slot| slot.stamp == stamp);
    if !current {
        cache.insert(
            path.to_path_buf(),
            CacheSlot {
                stamp,
                archive: Archive::parse(decompress(&std::fs::read(path).ok()?)),
            },
        );
    }
    cache.get(path).map(|slot| &slot.archive)
}

fn take_archive(path: &Path, cache: &mut HashMap<PathBuf, CacheSlot>) -> Archive {
    if let Some(stamp) = file_stamp(path) {
        if let Some(slot) = cache.remove(path) {
            if slot.stamp == stamp {
                return slot.archive;
            }
        }
        return Archive::parse(decompress(
            &std::fs::read(path).expect("read class-dump archive"),
        ));
    }
    cache.remove(path);
    Archive::from_parts(BTreeMap::new(), BTreeMap::new())
}

fn file_stamp(path: &Path) -> Option<Stamp> {
    let meta = std::fs::metadata(path).ok()?;
    let modified = meta.modified().ok()?;
    let since = modified.duration_since(SystemTime::UNIX_EPOCH).ok()?;
    Some((since.as_secs(), since.subsec_nanos(), meta.len()))
}

fn archive_path(root: &Path) -> PathBuf {
    root.join("recorded-bytes.zz")
}

fn dumps_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests")
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
    let mut raw = Vec::new();
    ZlibDecoder::new(bytes)
        .read_to_end(&mut raw)
        .expect("decompress class dump");
    raw
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

fn parse_range(token: &str) -> Option<(Channel, KotlinVersion, Option<KotlinVersion>)> {
    let (lo, hi) = match token.split_once("..") {
        Some((lo, "")) => (lo, None),
        Some((lo, hi)) => (lo, Some(hi)),
        None => (token, Some(token)),
    };
    let (channel, lo) = parse_bound(lo)?;
    let hi = match hi {
        None => None,
        Some(hi) => {
            let (hi_channel, hi) = parse_bound(hi)?;
            if hi_channel != channel {
                return None;
            }
            Some(hi)
        }
    };
    Some((channel, lo, hi))
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

fn render_span(span: Span) -> String {
    let bound = |version: KotlinVersion| match span.channel {
        Channel::Release => version.to_string(),
        Channel::Rc => format!("{version}-RC"),
    };
    let range = match span.hi {
        None => format!("{}..", bound(span.lo)),
        Some(hi) if hi == span.lo => bound(span.lo),
        Some(hi) => format!("{}..{}", bound(span.lo), bound(hi)),
    };
    format!("{range} {} {}", hex128(span.fingerprint), hex128(span.blob))
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
    std::env::var_os("KRUSTY_RECORD").is_some_and(|flag| flag == "1")
}

thread_local! {
    static LIVE_KOTLINC: Cell<bool> = const { Cell::new(false) };
}

/// Run `body` as a diagnostic comparison. Those calls need kotlinc's stderr, which the class
/// archive does not store, so they compile even for a release that already has dumps.
pub fn with_live_kotlinc<T>(body: impl FnOnce() -> T) -> T {
    LIVE_KOTLINC.with(|flag| {
        let previous = flag.replace(true);
        let value = body();
        flag.set(previous);
        value
    })
}

fn live_kotlinc() -> bool {
    LIVE_KOTLINC.with(Cell::get)
}

const INVOCATION_MODULE: &str = "_invocations";
const JAR_ENTRY: &str = "__jar__";
const EXIT_ENTRY: &str = "__exit__";
const STDERR_ENTRY: &str = "__stderr__";

pub(crate) struct ReplayedClasses {
    pub(crate) code: i32,
    pub(crate) stderr: String,
    pub(crate) files: BTreeMap<String, Vec<u8>>,
}

struct Invocation {
    out: PathBuf,
    fingerprint: u128,
    label: String,
}

/// Class files, exit code, and stderr recorded for this `kotlinc` invocation.
///
/// `Some` must not compile. `None` means the caller compiles: `KRUSTY_RECORD=1`, a live
/// diagnostic comparison, or a compiler that does not use the archive. A release or RC with no
/// matching dump panics. A dump written before exit codes were stored replays as a successful
/// compile with empty stderr.
pub fn replay_class_dump(args: &[String]) -> Option<ReplayedClasses> {
    if live_kotlinc() {
        return None;
    }
    let Some(compiler) = compiler_dump_version() else {
        return None;
    };
    if record_forced() {
        return None;
    }
    let Some(invocation) = parse_invocation(args) else {
        refuse_missing_dump(
            "a kotlinc invocation that does not name an output directory and source files",
        );
    };
    let key = hex128(invocation.fingerprint);
    if let Some(files) = load_files(
        &dumps_root(),
        INVOCATION_MODULE,
        &key,
        compiler,
        invocation.fingerprint,
    ) {
        return Some(split_replay(files));
    }
    refuse_missing_dump(&format!("sources {} fingerprint {}", invocation.label, key));
}

/// Write a replayed dump into the invocation's `-d` path.
pub fn write_replayed_classes(args: &[String], files: &BTreeMap<String, Vec<u8>>) {
    let invocation = parse_invocation(args).expect("replayed class dump names an output directory");
    write_output(&invocation.out, files);
}

/// Store the class files, exit code, and stderr produced while `KRUSTY_RECORD=1`. A rejected
/// compile is stored too, so the next run can replay it. A live diagnostic comparison and a
/// non-release compiler do not write the archive.
pub fn remember_class_dump(args: &[String], code: i32, stderr: &str) {
    if live_kotlinc() || !record_forced() || !ci_allows_write() {
        return;
    }
    let Some(compiler) = compiler_dump_version() else {
        return;
    };
    let Some(invocation) = parse_invocation(args) else {
        return;
    };
    let mut files = read_output_tree(&invocation.out).unwrap_or_default();
    attach_status(&mut files, code, stderr);
    store_files(
        &dumps_root(),
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
        files,
    }
}

fn exit_code(bytes: &[u8]) -> i32 {
    let Ok(bytes) = <[u8; 4]>::try_from(bytes) else {
        refuse_missing_dump("a kotlinc exit code that is not four bytes");
    };
    i32::from_le_bytes(bytes)
}

fn parse_invocation(args: &[String]) -> Option<Invocation> {
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
            let bytes = std::fs::read(arg).ok()?;
            let name = basename(arg);
            sources.push((name, bytes));
            index += 1;
            continue;
        }
        flags.push(normalize_invocation_flag(arg));
        index += 1;
    }
    let out = out?;
    if sources.is_empty() {
        return None;
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
    let classpath = classpath_content_fingerprint(&classpath);
    let flags = flags.join("\n");
    Some(Invocation {
        out,
        fingerprint: fingerprint_parts(&[blob.as_slice(), flags.as_bytes(), classpath.as_bytes()]),
        label,
    })
}

fn is_source_arg(arg: &str) -> bool {
    let path = Path::new(arg);
    let ext = path.extension().and_then(|ext| ext.to_str());
    matches!(ext, Some("kt" | "kts" | "java")) && path.is_file()
}

fn normalize_invocation_flag(arg: &str) -> String {
    if let Some(path) = arg.strip_prefix("-Xplugin=") {
        let bytes = std::fs::read(path).unwrap_or_default();
        return format!("-Xplugin={}", hex128(fingerprint_parts(&[&bytes])));
    }
    // A flag whose value is a scratch path (`-Xcommon-sources=/tmp/.../A.kt`) names the same
    // inputs wherever the files were written. Hash those bytes and keep every other flag literal.
    let Some((name, value)) = arg.split_once('=') else {
        return arg.to_string();
    };
    if value.is_empty() || !value.split(',').all(|part| Path::new(part).is_file()) {
        return arg.to_string();
    }
    let hashed = value
        .split(',')
        .map(|part| {
            let bytes = std::fs::read(part).unwrap_or_default();
            hex128(fingerprint_parts(&[&bytes]))
        })
        .collect::<Vec<_>>()
        .join(",");
    format!("{name}={hashed}")
}

fn classpath_content_fingerprint(paths: &[PathBuf]) -> String {
    let mut rows: Vec<String> = paths
        .iter()
        .map(|path| {
            if path.is_dir() {
                format!("dir:{:016x}", hash_tree(path))
            } else {
                let bytes = std::fs::read(path).unwrap_or_default();
                format!("file:{:032x}", fingerprint_parts(&[&bytes]))
            }
        })
        .collect();
    rows.sort();
    rows.join("\n")
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
mod tests {
    use super::*;

    fn version(text: &str) -> DumpVersion {
        parse_dump_version(text).unwrap_or_else(|| panic!("dump version {text}"))
    }

    fn files(bytes: &[u8]) -> BTreeMap<String, Vec<u8>> {
        let mut map = BTreeMap::new();
        map.insert("pkg/A".to_string(), bytes.to_vec());
        map
    }

    #[test]
    fn only_releases_and_rc_tags_are_cached() {
        for text in [
            "2.4.20",
            "2.4.20-release-482",
            "2.4.20-RC",
            "2.4.20-RC2",
            "2.4.0-RC-137",
            "2.4.20-RC2-release-15",
            "2.4.20-RC2-15",
        ] {
            assert_eq!(
                cacheable_build_identity(text).as_deref(),
                Some(text),
                "{text}"
            );
        }
        for text in [
            "",
            "2.4",
            "2.4.20-SNAPSHOT",
            "2.3.255-SNAPSHOT",
            "2.4.20-dev-2181",
            "2.4.20-Beta1",
            "2.4.20-Beta1-release-3",
            "2.4.20-RC2-beta",
            "v2.4.20",
            "2.4.20-release",
        ] {
            assert_eq!(cacheable_build_identity(text), None, "{text}");
        }
        assert_eq!(
            parse_dump_version("2.4.20-release-482"),
            Some(DumpVersion {
                version: KotlinVersion::new(2, 4, 20),
                channel: Channel::Release,
            })
        );
        assert_eq!(
            parse_dump_version("2.4.20-RC2"),
            Some(DumpVersion {
                version: KotlinVersion::new(2, 4, 20),
                channel: Channel::Rc,
            })
        );
    }

    #[test]
    fn an_open_range_covers_a_newer_release_without_rewriting() {
        let root = temp_root("open");
        let fingerprint = fingerprint_parts(&[b"source"]);
        let release = version("2.4.20");
        store_files(
            &root,
            "mod",
            "case|Stem|default|plain",
            release,
            fingerprint,
            &files(b"one"),
        );
        let index = disk_index(&root);
        assert!(index.contains("[[mod]]"), "{index}");
        assert!(index.contains("2.4.20.. "), "{index}");
        assert!(archive_path(&root).is_file());
        assert!(!index.contains("2.4.10"), "{index}");
        let newer = DumpVersion {
            version: KotlinVersion::new(2, 4, 30),
            channel: Channel::Release,
        };
        let hit = load_files(&root, "mod", "case|Stem|default|plain", newer, fingerprint);
        assert_eq!(hit.unwrap().get("pkg/A").unwrap(), b"one");
        store_files(
            &root,
            "mod",
            "case|Stem|default|plain",
            newer,
            fingerprint,
            &files(b"one"),
        );
        let again = disk_index(&root);
        assert_eq!(
            again, index,
            "matching bytes leave the open range untouched"
        );

        store_files(
            &root,
            "mod",
            "case|Stem|default|plain",
            newer,
            fingerprint,
            &files(b"two"),
        );
        let split = disk_index(&root);
        assert!(split.contains("2.4.20 "), "{split}");
        assert!(split.contains("2.4.30.. "), "{split}");
        assert_eq!(
            load_files(
                &root,
                "mod",
                "case|Stem|default|plain",
                release,
                fingerprint
            )
            .unwrap()
            .get("pkg/A")
            .unwrap(),
            b"one"
        );
        assert_eq!(
            load_files(&root, "mod", "case|Stem|default|plain", newer, fingerprint)
                .unwrap()
                .get("pkg/A")
                .unwrap(),
            b"two"
        );
        let rc = version("2.4.20-RC2");
        assert!(load_files(&root, "mod", "case|Stem|default|plain", rc, fingerprint).is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_release_dump_is_reused_and_a_snapshot_never_is() {
        let root = temp_root("reuse");
        let fingerprint = fingerprint_parts(&[b"source"]);
        let mut compiles = 0u32;
        let compile = |compiles: &mut u32| {
            *compiles += 1;
            Some(files(b"class-a"))
        };
        let query = |compiler, fingerprint, force| Recall {
            root: &root,
            module: "mod",
            key: "case|Stem|default|plain",
            compiler,
            fingerprint,
            force,
            write: true,
        };
        let first = recall(
            query(Some(version("2.4.20-RC2")), fingerprint, true),
            |_| true,
            || compile(&mut compiles),
        );
        assert_eq!(first.unwrap().get("pkg/A").unwrap(), b"class-a");
        assert_eq!(compiles, 1);
        let second = recall(
            query(Some(version("2.4.20-RC")), fingerprint, false),
            |_| true,
            || compile(&mut compiles),
        );
        assert_eq!(second.unwrap().get("pkg/A").unwrap(), b"class-a");
        assert_eq!(compiles, 1, "a matching dump skips kotlinc");

        let changed = fingerprint_parts(&[b"source-v2"]);
        let missing = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            recall(
                query(Some(version("2.4.20-RC2")), changed, false),
                |_| true,
                || compile(&mut compiles),
            )
        }));
        let message = panic_message(missing.expect_err("a new fingerprint must fail"));
        assert!(
            message.contains("tests/recorded-bytes.zz") && message.contains("KRUSTY_RECORD=1"),
            "{message}"
        );
        assert_eq!(compiles, 1, "a missing release dump does not compile");

        let snapshot = recall(
            query(None, changed, false),
            |_| true,
            || compile(&mut compiles),
        );
        assert!(snapshot.is_some());
        assert_eq!(compiles, 2);
        let again = recall(
            query(None, changed, false),
            |_| true,
            || compile(&mut compiles),
        );
        assert!(again.is_some());
        assert_eq!(compiles, 3, "a snapshot never reuses a dump");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_missing_release_dump_fails_without_compiling() {
        let root = temp_root("ci");
        let fingerprint = fingerprint_parts(&[b"source"]);
        let mut compiled = false;
        let missing = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            recall(
                Recall {
                    root: &root,
                    module: "mod",
                    key: "case|Stem|default|plain",
                    compiler: Some(version("2.4.20-release-1")),
                    fingerprint,
                    force: false,
                    write: false,
                },
                |_| true,
                || {
                    compiled = true;
                    Some(files(b"bytes"))
                },
            )
        }));
        let message = panic_message(missing.expect_err("a missing dump must fail the test"));
        assert!(message.contains("KRUSTY_RECORD=1"), "{message}");
        assert!(!compiled, "a missing release dump does not compile");
        assert!(
            !archive_path(&root).exists(),
            "a miss that is not recorded leaves no dump"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn distinct_class_sets_do_not_share_a_dump_key() {
        assert_ne!(classes_suffix(&["pkg/A"]), classes_suffix(&["pkg/B"]));
        assert_eq!(classes_suffix(&["pkg/B", "pkg/A"]), "#pkg/A,pkg/B");
        assert_ne!(classes_suffix(&["pkg/A"]), "#tree");
    }

    fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
        payload
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| {
                payload
                    .downcast_ref::<&str>()
                    .map(|text| (*text).to_string())
            })
            .unwrap_or_default()
    }

    #[test]
    fn a_hit_missing_a_requested_class_fails_until_recorded() {
        let root = temp_root("missing");
        let fingerprint = fingerprint_parts(&[b"source"]);
        let release = version("2.4.20");
        let key = "case|Stem|default|plain";
        store_files(&root, "mod", key, release, fingerprint, &files(b"one"));
        let mut compiles = 0u32;
        let mut query = Recall {
            root: &root,
            module: "mod",
            key,
            compiler: Some(release),
            fingerprint,
            force: false,
            write: true,
        };
        let missing = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            recall(
                query,
                |hit| hit.contains_key("pkg/Missing"),
                || {
                    compiles += 1;
                    Some(files(b"unused"))
                },
            )
        }));
        assert!(missing.is_err(), "an incomplete dump is not a hit");
        assert_eq!(compiles, 0, "an incomplete dump does not compile");
        query.force = true;
        let replaced = recall(
            query,
            |hit| hit.contains_key("pkg/Missing"),
            || {
                compiles += 1;
                let mut map = files(b"two");
                map.insert("pkg/Missing".to_string(), b"present".to_vec());
                Some(map)
            },
        );
        assert_eq!(compiles, 1);
        assert_eq!(replaced.unwrap().get("pkg/Missing").unwrap(), b"present");
        query.force = false;
        let reused = recall(
            query,
            |hit| hit.contains_key("pkg/Missing"),
            || {
                compiles += 1;
                Some(files(b"three"))
            },
        );
        assert_eq!(compiles, 1, "the completed dump is reused");
        assert_eq!(reused.unwrap().get("pkg/A").unwrap(), b"two");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn an_invocation_fingerprint_ignores_the_output_directory() {
        let root = temp_root("inv");
        let source = root.join("Lib.kt");
        std::fs::write(&source, "fun box() = \"OK\"\n").unwrap();
        let args = |out: &str| {
            vec![
                "-d".to_string(),
                root.join(out).to_string_lossy().into_owned(),
                source.to_string_lossy().into_owned(),
            ]
        };
        let left = parse_invocation(&args("out-a")).expect("left invocation");
        let right = parse_invocation(&args("out-b")).expect("right invocation");
        assert_eq!(left.fingerprint, right.fingerprint);
        assert_ne!(left.out, right.out);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_common_sources_flag_ignores_its_scratch_path() {
        let root = temp_root("common-src");
        let left_dir = root.join("left");
        let right_dir = root.join("right");
        std::fs::create_dir_all(&left_dir).unwrap();
        std::fs::create_dir_all(&right_dir).unwrap();
        let text = "expect class A\n";
        std::fs::write(left_dir.join("Common.kt"), text).unwrap();
        std::fs::write(right_dir.join("Common.kt"), text).unwrap();
        let flag = |dir: &Path| {
            format!(
                "-Xcommon-sources={},{}",
                dir.join("Common.kt").display(),
                dir.join("Common.kt").display()
            )
        };
        assert_eq!(
            normalize_invocation_flag(&flag(&left_dir)),
            normalize_invocation_flag(&flag(&right_dir))
        );
        assert_ne!(flag(&left_dir), flag(&right_dir));
        std::fs::write(right_dir.join("Common.kt"), "expect class B\n").unwrap();
        assert_ne!(
            normalize_invocation_flag(&flag(&left_dir)),
            normalize_invocation_flag(&flag(&right_dir))
        );
        assert_eq!(
            normalize_invocation_flag("-jvm-target=17"),
            "-jvm-target=17"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn two_fingerprints_under_one_key_both_replay() {
        let root = temp_root("two-fp");
        let release = version("2.4.20");
        let first = fingerprint_parts(&[b"first"]);
        let second = fingerprint_parts(&[b"second"]);
        store_files(
            &root,
            "mod",
            "case|Stem|default|plain#tree",
            release,
            first,
            &files(b"one"),
        );
        store_files(
            &root,
            "mod",
            "case|Stem|default|plain#tree",
            release,
            second,
            &files(b"two"),
        );
        assert_eq!(
            load_files(&root, "mod", "case|Stem|default|plain#tree", release, first)
                .unwrap()
                .get("pkg/A")
                .unwrap(),
            b"one"
        );
        assert_eq!(
            load_files(
                &root,
                "mod",
                "case|Stem|default|plain#tree",
                release,
                second
            )
            .unwrap()
            .get("pkg/A")
            .unwrap(),
            b"two"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_rejected_invocation_replays_its_stderr_and_a_plain_dump_is_success() {
        let mut rejected = BTreeMap::new();
        rejected.insert("pkg/A.class".to_string(), b"class".to_vec());
        attach_status(&mut rejected, 1, "only named arguments");
        let replayed = split_replay(rejected);
        assert_eq!(replayed.code, 1);
        assert_eq!(replayed.stderr, "only named arguments");
        assert_eq!(replayed.files.get("pkg/A.class").unwrap(), b"class");
        assert!(!replayed.files.contains_key(EXIT_ENTRY));
        assert!(!replayed.files.contains_key(STDERR_ENTRY));

        let mut plain = BTreeMap::new();
        plain.insert("pkg/A.class".to_string(), b"class".to_vec());
        let replayed = split_replay(plain);
        assert_eq!(replayed.code, 0);
        assert_eq!(replayed.stderr, "");
        assert_eq!(replayed.files.get("pkg/A.class").unwrap(), b"class");

        let root = temp_root("status");
        let out = root.join("out");
        let mut stored = BTreeMap::new();
        stored.insert("pkg/A.class".to_string(), b"class".to_vec());
        attach_status(&mut stored, 0, "");
        write_output(&out, &stored);
        assert_eq!(std::fs::read(out.join("pkg/A.class")).unwrap(), b"class");
        assert!(!out.join(EXIT_ENTRY).exists());
        assert!(!out.join(STDERR_ENTRY).exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_directory_classpath_ignores_its_own_name() {
        let root = temp_root("cp");
        for name in ["pid-111", "pid-222"] {
            let class = root.join(name).join("pkg").join("A.class");
            std::fs::create_dir_all(class.parent().unwrap()).unwrap();
            std::fs::write(&class, b"same-bytes").unwrap();
        }
        let left = class_dump_inputs("src", "default", &[], &[root.join("pid-111")]);
        let right = class_dump_inputs("src", "default", &[], &[root.join("pid-222")]);
        assert_eq!(left.fingerprint, right.fingerprint);
        let module = root
            .join("pid-111")
            .join("META-INF")
            .join("main.kotlin_module");
        std::fs::create_dir_all(module.parent().unwrap()).unwrap();
        std::fs::write(&module, b"idx").unwrap();
        let shifted = class_dump_inputs("src", "default", &[], &[root.join("pid-111")]);
        assert_ne!(left.fingerprint, shifted.fingerprint);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_blob_round_trips_and_is_smaller_than_the_class_bytes() {
        let payload = b"kotlin/Metadata".repeat(200);
        let mut map = BTreeMap::new();
        map.insert("pkg/A.class".to_string(), payload.clone());
        map.insert(
            "META-INF/main.kotlin_module".to_string(),
            b"module".to_vec(),
        );
        let mut second = BTreeMap::new();
        second.insert("pkg/B.class".to_string(), payload);
        let archive = Archive::from_parts(
            BTreeMap::new(),
            [(1u128, encode_raw(&map)), (2, encode_raw(&second))]
                .into_iter()
                .collect(),
        );
        assert!(compress(&archive.body).len() < archive.body.len());
        assert_eq!(decode_raw(archive.blob(1).unwrap()), Some(map));
        assert_eq!(decode_raw(archive.blob(2).unwrap()), Some(second));
    }

    #[test]
    fn every_module_shares_one_archive() {
        let root = temp_root("one");
        let fingerprint = fingerprint_parts(&[b"source"]);
        let release = version("2.4.20");
        store_files(
            &root,
            "mod",
            "case|A|default|plain",
            release,
            fingerprint,
            &files(b"a"),
        );
        store_files(
            &root,
            "other",
            "case|B|default|plain",
            release,
            fingerprint,
            &files(b"b"),
        );
        let index = disk_index(&root);
        assert!(index.contains("[[mod]]"), "{index}");
        assert!(index.contains("[[other]]"), "{index}");
        assert_eq!(
            std::fs::read_dir(&root).unwrap().count(),
            1,
            "modules share one archive file"
        );
        assert_eq!(
            load_files(&root, "mod", "case|A|default|plain", release, fingerprint)
                .unwrap()
                .get("pkg/A")
                .unwrap(),
            b"a"
        );
        assert_eq!(
            load_files(&root, "other", "case|B|default|plain", release, fingerprint)
                .unwrap()
                .get("pkg/A")
                .unwrap(),
            b"b"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_committed_dump_archive_loads() {
        let root = dumps_root();
        let path = archive_path(&root);
        assert!(path.is_file(), "class dumps are one archive");
        assert!(!root.join("recorded-bytes").exists());
        let archive = Archive::parse(decompress(&std::fs::read(&path).unwrap()));
        let entries = archive
            .modules
            .get("value_class_text_e2e")
            .expect("recorded module");
        let (key, spans) = entries
            .iter()
            .find(|(key, _)| key.contains("#Holder"))
            .expect("holder class dump");
        assert!(spans[0].hi.is_none(), "the newest release range stays open");
        let version = DumpVersion {
            version: KotlinVersion::new(2, 4, 30),
            channel: Channel::Release,
        };
        let hit = load_files(
            &root,
            "value_class_text_e2e",
            key,
            version,
            spans[0].fingerprint,
        )
        .expect("an open range covers a newer release");
        assert!(hit.keys().any(|name| name.contains("Holder")));
    }

    fn disk_index(root: &Path) -> String {
        let raw = decompress(&std::fs::read(archive_path(root)).unwrap());
        let end = raw.iter().position(|byte| *byte == 0).unwrap();
        String::from_utf8(raw[..end].to_vec()).unwrap()
    }

    fn temp_root(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "krusty-byte-dump-{label}-{}-{}",
            std::process::id(),
            fingerprint_parts(&[label.as_bytes()])
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }
}
