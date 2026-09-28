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
//! range. The bytes themselves are zlib blobs addressed by their content, one blob per distinct
//! output, referenced from a small text index per test module.
//!
//! `KRUSTY_RECORD=1` ignores a stored dump and recompiles. Under CI a missing dump still compiles
//! with kotlinc, and the result is not written: CI does not bless dumps nobody committed.
//!
//! Dumps are ordinary files under `tests/recorded-bytes/`, read at runtime. They are not compiled
//! into the test binary.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

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
/// `compile` runs only on a miss (or `KRUSTY_RECORD=1`) and returns the internal-name → bytes map
/// of that compile. `None` from `compile`, or a requested class absent from its map, yields `None`.
/// A non-release compiler always takes `compile` and does not touch the dump directory.
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
/// range whose bytes already match does not rewrite the index.
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
    let cached = if query.force {
        None
    } else {
        load_files(
            query.root,
            query.module,
            query.key,
            compiler,
            query.fingerprint,
        )
        .filter(|hit| accept(hit))
    };
    if let Some(hit) = cached {
        return Some(hit);
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
    let text = std::fs::read_to_string(index_path(root, module)).ok()?;
    let entries = parse_index(&text);
    let span = entries.get(key).and_then(|spans| {
        spans
            .iter()
            .filter(|span| {
                span.channel == compiler.channel
                    && span.fingerprint == fingerprint
                    && span_contains(span, compiler.version)
            })
            .max_by_key(|span| span.lo)
    })?;
    decode_files(&std::fs::read(blob_path(root, span.blob)).ok()?)
}

fn store_files(
    root: &Path,
    module: &str,
    key: &str,
    compiler: DumpVersion,
    fingerprint: u128,
    files: &BTreeMap<String, Vec<u8>>,
) {
    let blob = blob_id(files);
    write_blob(root, blob, files);
    let dir = dumps_root_of(root);
    std::fs::create_dir_all(&dir).expect("create tests/recorded-bytes");
    let lock = std::fs::File::open(&dir).expect("open class-dump directory");
    // SAFETY: `flock` on a descriptor this function owns until it returns.
    let locked = unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) };
    assert_eq!(locked, 0, "lock class-dump directory");

    let path = index_path(root, module);
    let previous = std::fs::read_to_string(&path).unwrap_or_default();
    let mut entries = parse_index(&previous);
    let spans = entries.entry(key.to_string()).or_default();
    let mut versions = KotlinVersion::supported();
    if !versions.contains(&compiler.version) {
        versions.push(compiler.version);
        versions.sort();
    }
    let mut projected: Vec<Option<Recorded>> = versions
        .iter()
        .map(|version| {
            spans
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
    let retained = spans
        .iter()
        .copied()
        .filter(|span| span.channel != compiler.channel)
        .chain(updated)
        .collect();
    *spans = retained;
    let rendered = render_index(&entries);
    if rendered != previous {
        static TEMP: AtomicU64 = AtomicU64::new(0);
        let staging = path.with_extension(format!(
            "txt.{}.{}",
            std::process::id(),
            TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        if let Some(parent) = staging.parent() {
            std::fs::create_dir_all(parent).expect("create class-dump index directory");
        }
        std::fs::write(&staging, &rendered).expect("write class-dump index");
        std::fs::rename(&staging, &path).expect("replace class-dump index");
    }
    drop(lock);
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

fn index_path(root: &Path, module: &str) -> PathBuf {
    dumps_root_of(root)
        .join("m")
        .join(format!("{}.txt", sanitize(module)))
}

fn dumps_root_of(root: &Path) -> PathBuf {
    root.to_path_buf()
}

fn dumps_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/recorded-bytes")
}

fn blob_path(root: &Path, blob: u128) -> PathBuf {
    let hex = hex128(blob);
    dumps_root_of(root)
        .join("b")
        .join(&hex[..2])
        .join(format!("{}.zz", &hex[2..]))
}

fn write_blob(root: &Path, blob: u128, files: &BTreeMap<String, Vec<u8>>) {
    let path = blob_path(root, blob);
    if path.is_file() {
        return;
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create class-dump blob directory");
    }
    let compressed = encode_files(files);
    let staging = path.with_extension("zz.tmp");
    std::fs::write(&staging, compressed).expect("write class-dump blob");
    match std::fs::rename(&staging, &path) {
        Ok(()) => {}
        Err(_) if path.is_file() => {
            let _ = std::fs::remove_file(&staging);
        }
        Err(error) => panic!("publish class-dump blob: {error}"),
    }
}

fn blob_id(files: &BTreeMap<String, Vec<u8>>) -> u128 {
    fingerprint_parts(&[&encode_raw(files)])
}

fn encode_files(files: &BTreeMap<String, Vec<u8>>) -> Vec<u8> {
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::new(9));
    encoder
        .write_all(&encode_raw(files))
        .expect("compress class dump");
    encoder.finish().expect("finish class dump")
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

fn decode_files(compressed: &[u8]) -> Option<BTreeMap<String, Vec<u8>>> {
    let mut raw = Vec::new();
    ZlibDecoder::new(compressed).read_to_end(&mut raw).ok()?;
    let mut files = BTreeMap::new();
    let mut offset = 0;
    while offset < raw.len() {
        let name_len = read_u32(&raw, &mut offset)? as usize;
        let name = std::str::from_utf8(read_bytes(&raw, &mut offset, name_len)?).ok()?;
        let data_len = read_u32(&raw, &mut offset)? as usize;
        let data = read_bytes(&raw, &mut offset, data_len)?.to_vec();
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

fn parse_index(text: &str) -> BTreeMap<String, Vec<Span>> {
    let mut entries: BTreeMap<String, Vec<Span>> = BTreeMap::new();
    let mut key: Option<String> = None;
    for (index, line) in text.lines().enumerate() {
        let malformed =
            || -> ! { panic!("malformed class-dump index line {}: {line:?}", index + 1) };
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(name) = line
            .strip_prefix('[')
            .and_then(|line| line.strip_suffix(']'))
        {
            entries.entry(name.to_string()).or_default();
            key = Some(name.to_string());
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
        match key.as_ref().and_then(|key| entries.get_mut(key)) {
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
    entries
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

fn render_index(entries: &BTreeMap<String, Vec<Span>>) -> String {
    let mut text = String::from(
        "# kotlinc class dumps. An open range (2.4.20..) covers that release and every newer one.\n\
         # RC tags share a separate range (2.4.20-RC..). A snapshot compiler never uses this file.\n",
    );
    for (key, spans) in entries {
        text.push_str(&format!("\n[{key}]\n"));
        let mut ordered = spans.clone();
        ordered.sort_by_key(|span| (span.channel == Channel::Rc, span.lo));
        for span in ordered {
            text.push_str(&render_span(span));
            text.push('\n');
        }
    }
    text
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
        let index = std::fs::read_to_string(index_path(&root, "mod")).unwrap();
        assert!(index.contains("2.4.20.. "), "{index}");
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
        let again = std::fs::read_to_string(index_path(&root, "mod")).unwrap();
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
        let split = std::fs::read_to_string(index_path(&root, "mod")).unwrap();
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
        let query = |compiler, fingerprint| Recall {
            root: &root,
            module: "mod",
            key: "case|Stem|default|plain",
            compiler,
            fingerprint,
            force: false,
            write: true,
        };
        let first = recall(
            query(Some(version("2.4.20-RC2")), fingerprint),
            |_| true,
            || compile(&mut compiles),
        );
        assert_eq!(first.unwrap().get("pkg/A").unwrap(), b"class-a");
        assert_eq!(compiles, 1);
        let second = recall(
            query(Some(version("2.4.20-RC")), fingerprint),
            |_| true,
            || compile(&mut compiles),
        );
        assert_eq!(second.unwrap().get("pkg/A").unwrap(), b"class-a");
        assert_eq!(compiles, 1, "a matching dump skips kotlinc");

        let changed = fingerprint_parts(&[b"source-v2"]);
        let third = recall(
            query(Some(version("2.4.20-RC2")), changed),
            |_| true,
            || compile(&mut compiles),
        );
        assert_eq!(third.unwrap().get("pkg/A").unwrap(), b"class-a");
        assert_eq!(compiles, 2, "a new fingerprint recompiles");

        let snapshot = recall(query(None, changed), |_| true, || compile(&mut compiles));
        assert!(snapshot.is_some());
        assert_eq!(compiles, 3);
        let again = recall(query(None, changed), |_| true, || compile(&mut compiles));
        assert!(again.is_some());
        assert_eq!(compiles, 4, "a snapshot never reuses a dump");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn ci_does_not_write_a_missing_dump() {
        let root = temp_root("ci");
        let fingerprint = fingerprint_parts(&[b"source"]);
        let got = recall(
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
            || Some(files(b"bytes")),
        );
        assert_eq!(got.unwrap().get("pkg/A").unwrap(), b"bytes");
        assert!(
            !index_path(&root, "mod").exists(),
            "a miss that is not allowed to record leaves no dump"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn distinct_class_sets_do_not_share_a_dump_key() {
        assert_ne!(classes_suffix(&["pkg/A"]), classes_suffix(&["pkg/B"]));
        assert_eq!(classes_suffix(&["pkg/B", "pkg/A"]), "#pkg/A,pkg/B");
        assert_ne!(classes_suffix(&["pkg/A"]), "#tree");
    }

    #[test]
    fn a_hit_missing_a_requested_class_is_recompiled() {
        let root = temp_root("missing");
        let fingerprint = fingerprint_parts(&[b"source"]);
        let release = version("2.4.20");
        let key = "case|Stem|default|plain";
        store_files(&root, "mod", key, release, fingerprint, &files(b"one"));
        let mut compiles = 0u32;
        let query = Recall {
            root: &root,
            module: "mod",
            key,
            compiler: Some(release),
            fingerprint,
            force: false,
            write: true,
        };
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
        assert_eq!(compiles, 1, "an incomplete dump is not a hit");
        assert_eq!(replaced.unwrap().get("pkg/Missing").unwrap(), b"present");
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
        let mut map = BTreeMap::new();
        map.insert("pkg/A.class".to_string(), vec![0u8; 4096]);
        map.insert(
            "META-INF/main.kotlin_module".to_string(),
            b"module".to_vec(),
        );
        let compressed = encode_files(&map);
        assert!(compressed.len() < 4096);
        assert_eq!(decode_files(&compressed), Some(map));
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
