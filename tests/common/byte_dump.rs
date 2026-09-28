//! Kotlinc class-file dumps for byte-equality checks.
//!
//! A check compares krusty against a dump recorded from kotlinc for this fixture, and runs kotlinc
//! only when the dump is missing or its fingerprint no longer matches the fixture. The dump is
//! keyed by the compiler's `build.txt` identity. A release (`2.4.20`, `2.4.20-release-482`) and an
//! RC tag (`2.4.20-RC`, `2.4.20-RC2`, `2.4.0-RC-137`) are immutable published builds, so their
//! dumps are safe to reuse. A snapshot, dev, or beta build is not: the same version string moves,
//! and those compilers always compile for real and never read or write a dump.
//!
//! `KRUSTY_RECORD=1` ignores a stored dump and recompiles. Under CI a missing dump still compiles
//! with kotlinc, and the result is not written: CI does not bless dumps nobody committed.
//!
//! Dumps are ordinary files under `tests/recorded-bytes/`, read at runtime. They are not compiled
//! into the test binary.

use std::collections::BTreeMap;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// The published compiler identity, when this process's kotlinc is a release or an RC tag.
///
/// `None` for a missing dist, a snapshot, a dev build, a beta, or any other non-release string.
/// The value is `build.txt`'s first line, unchanged, so two builds of one version do not share
/// dumps.
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
    let build_id = published_compiler_id();
    cached_at(
        CacheQuery {
            root: &dumps_root(),
            build_id: build_id.as_deref(),
            module: &module,
            case: &case,
            stem,
            jvm_target,
            variant,
            fingerprint,
            classes,
            force: record_forced(),
            write: ci_allows_write(),
        },
        compile,
    )
}

struct CacheQuery<'a> {
    root: &'a Path,
    build_id: Option<&'a str>,
    module: &'a str,
    case: &'a str,
    stem: &'a str,
    jvm_target: &'a str,
    variant: &'a str,
    fingerprint: u128,
    classes: &'a [&'a str],
    force: bool,
    write: bool,
}

fn cached_at(
    mut query: CacheQuery<'_>,
    compile: impl FnOnce() -> Option<BTreeMap<String, Vec<u8>>>,
) -> Option<Vec<Vec<u8>>> {
    query.write = query.write && query.build_id.is_some();
    if query.build_id.is_some() && !query.force {
        if let Some(hit) = load_all(&query) {
            return Some(hit);
        }
    }
    let produced = compile()?;
    let owned = query
        .classes
        .iter()
        .map(|class| produced.get(*class).cloned())
        .collect::<Option<Vec<_>>>()?;
    if query.write {
        for (class, bytes) in query.classes.iter().zip(&owned) {
            write_dump(&query, class, bytes);
        }
    }
    Some(owned)
}

fn load_all(query: &CacheQuery<'_>) -> Option<Vec<Vec<u8>>> {
    let mut loaded = Vec::with_capacity(query.classes.len());
    for class in query.classes {
        loaded.push(read_dump(&class_path(query, class), query.fingerprint)?);
    }
    Some(loaded)
}

fn read_dump(class_path: &Path, fingerprint: u128) -> Option<Vec<u8>> {
    let recorded = std::fs::read_to_string(class_path.with_extension("fp")).ok()?;
    let recorded = recorded.trim();
    if recorded != format!("{fingerprint:032x}") {
        return None;
    }
    std::fs::read(class_path).ok()
}

fn write_dump(query: &CacheQuery<'_>, class: &str, bytes: &[u8]) {
    let class_path = class_path(query, class);
    let dir = class_path
        .parent()
        .expect("a class dump path has a parent")
        .to_path_buf();
    std::fs::create_dir_all(&dir).expect("create tests/recorded-bytes");
    let lock = std::fs::File::open(&dir).expect("open class-dump directory");
    // SAFETY: `flock` on a descriptor this function owns for the duration of the call.
    let locked = unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) };
    assert_eq!(locked, 0, "lock class-dump directory");
    static TEMP: AtomicU64 = AtomicU64::new(0);
    let stamp = format!(
        "{}.{}",
        std::process::id(),
        TEMP.fetch_add(1, Ordering::Relaxed)
    );
    let tmp_class = class_path.with_extension(format!("class.{stamp}.tmp"));
    let fp_path = class_path.with_extension("fp");
    let tmp_fp = class_path.with_extension(format!("fp.{stamp}.tmp"));
    std::fs::write(&tmp_class, bytes).expect("write class dump");
    std::fs::write(&tmp_fp, format!("{:032x}\n", query.fingerprint))
        .expect("write class fingerprint");
    std::fs::rename(&tmp_class, &class_path).expect("replace class dump");
    std::fs::rename(&tmp_fp, &fp_path).expect("replace class fingerprint");
}

fn class_path(query: &CacheQuery<'_>, class: &str) -> PathBuf {
    let mut path = query.root.join(sanitize(
        query.build_id.expect("a dump path requires a build id"),
    ));
    path.push(sanitize(query.module));
    path.push(sanitize(&query.case.replace("::", "__")));
    path.push(sanitize(query.stem));
    path.push(sanitize(query.jvm_target));
    path.push(variant_component(query.variant));
    for part in class.split('/') {
        path.push(sanitize(part));
    }
    path.set_extension("class");
    path
}

fn variant_component(variant: &str) -> String {
    if variant.is_empty() {
        "plain".to_string()
    } else {
        format!("{:016x}", fnv64(0xcbf29ce484222325, variant.as_bytes()))
    }
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

fn dumps_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/recorded-bytes")
}

fn record_forced() -> bool {
    std::env::var_os("KRUSTY_RECORD").is_some_and(|flag| flag == "1")
}

fn ci_allows_write() -> bool {
    std::env::var_os("CI").is_none()
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
    let Some(build_id) = published_compiler_id() else {
        return compile();
    };
    let dir = dumps_root()
        .join(sanitize(&build_id))
        .join(sanitize(&module))
        .join(sanitize(&case.replace("::", "__")))
        .join(sanitize(stem))
        .join(sanitize(jvm_target))
        .join(variant_component(variant))
        .join("all");
    let files = cached_tree(
        &dir,
        fingerprint,
        record_forced(),
        ci_allows_write(),
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

/// A content-addressed dump shared by every test that builds the same kotlinc output.
///
/// `None` when this compiler is not a release or RC, when `KRUSTY_RECORD=1`, or when the slot
/// has no complete dump. The slot is one path component under `<build.txt>/_libs/`.
pub fn load_shared_files(slot: &str, fingerprint: u128) -> Option<BTreeMap<String, Vec<u8>>> {
    if record_forced() {
        return None;
    }
    let build_id = published_compiler_id()?;
    load_tree(&shared_dir(&build_id, slot), fingerprint)
}

/// Record `files` (relative path → bytes, including `META-INF` entries) for [`load_shared_files`].
///
/// A no-op for a snapshot, dev, or beta compiler, and under CI.
pub fn store_shared_files(slot: &str, fingerprint: u128, files: &BTreeMap<String, Vec<u8>>) {
    let Some(build_id) = published_compiler_id() else {
        return;
    };
    if !ci_allows_write() {
        return;
    }
    publish_tree(&shared_dir(&build_id, slot), fingerprint, files);
}

fn shared_dir(build_id: &str, slot: &str) -> PathBuf {
    dumps_root()
        .join(sanitize(build_id))
        .join("_libs")
        .join(sanitize(slot))
}

fn cached_tree(
    dir: &Path,
    fingerprint: u128,
    force: bool,
    write: bool,
    compile: impl FnOnce() -> Option<BTreeMap<String, Vec<u8>>>,
) -> Option<BTreeMap<String, Vec<u8>>> {
    if !force {
        if let Some(hit) = load_tree(dir, fingerprint) {
            return Some(hit);
        }
    }
    let produced = compile()?;
    if write {
        publish_tree(dir, fingerprint, &produced);
    }
    Some(produced)
}

fn load_tree(dir: &Path, fingerprint: u128) -> Option<BTreeMap<String, Vec<u8>>> {
    let recorded = std::fs::read_to_string(dir.join("dump.fp")).ok()?;
    if recorded.trim() != format!("{fingerprint:032x}") {
        return None;
    }
    let manifest = std::fs::read_to_string(dir.join("dump.manifest")).ok()?;
    let mut files = BTreeMap::new();
    for relative in manifest.lines().filter(|line| !line.is_empty()) {
        files.insert(
            relative.to_string(),
            std::fs::read(stored_path(dir, relative)).ok()?,
        );
    }
    Some(files)
}

fn stored_path(dir: &Path, relative: &str) -> PathBuf {
    let mut path = dir.to_path_buf();
    for part in relative.split('/') {
        path.push(sanitize(part));
    }
    path
}

fn publish_tree(dir: &Path, fingerprint: u128, files: &BTreeMap<String, Vec<u8>>) {
    let parent = dir
        .parent()
        .expect("a class-dump tree has a parent")
        .to_path_buf();
    std::fs::create_dir_all(&parent).expect("create class-dump parent");
    let lock = std::fs::File::open(&parent).expect("open class-dump parent");
    // SAFETY: `flock` on a descriptor this function owns until it returns.
    let locked = unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) };
    assert_eq!(locked, 0, "lock class-dump parent");
    static TEMP: AtomicU64 = AtomicU64::new(0);
    let staging = parent.join(format!(
        ".staging-{}-{}",
        std::process::id(),
        TEMP.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&staging);
    write_tree_files(&staging, fingerprint, files);
    if dir.exists() {
        std::fs::remove_dir_all(dir).expect("replace class-dump tree");
    }
    if let Err(error) = std::fs::rename(&staging, dir) {
        let _ = std::fs::remove_dir_all(&staging);
        panic!("publish class-dump tree: {error}");
    }
}

fn write_tree_files(dir: &Path, fingerprint: u128, files: &BTreeMap<String, Vec<u8>>) {
    std::fs::create_dir_all(dir).expect("create class-dump staging");
    let mut manifest = String::new();
    for (relative, bytes) in files {
        let path = stored_path(dir, relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create class-dump package");
        }
        std::fs::write(&path, bytes).expect("write class dump");
        manifest.push_str(relative);
        manifest.push('\n');
    }
    std::fs::write(dir.join("dump.manifest"), manifest).expect("write class-dump manifest");
    std::fs::write(dir.join("dump.fp"), format!("{fingerprint:032x}\n"))
        .expect("write class-dump fingerprint");
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
    }

    #[test]
    fn a_release_dump_is_reused_and_a_snapshot_never_is() {
        let root = std::env::temp_dir().join(format!(
            "krusty-byte-dump-{}-{}",
            std::process::id(),
            fingerprint_parts(&[b"byte-dump-test"])
        ));
        let _ = std::fs::remove_dir_all(&root);
        let fingerprint = fingerprint_parts(&[b"source"]);
        let classes = ["pkg/A"];
        let mut compiles = 0u32;
        let compile = |compiles: &mut u32| {
            *compiles += 1;
            let mut map = BTreeMap::new();
            map.insert("pkg/A".to_string(), b"class-a".to_vec());
            Some(map)
        };
        let query = |fingerprint| CacheQuery {
            root: &root,
            build_id: Some("2.4.20-RC2"),
            module: "mod",
            case: "case",
            stem: "Stem",
            jvm_target: "default",
            variant: "",
            fingerprint,
            classes: &classes,
            force: false,
            write: true,
        };
        let first = cached_at(query(fingerprint), || compile(&mut compiles));
        assert_eq!(first.unwrap(), vec![b"class-a".to_vec()]);
        assert_eq!(compiles, 1);
        let second = cached_at(query(fingerprint), || compile(&mut compiles));
        assert_eq!(second.unwrap(), vec![b"class-a".to_vec()]);
        assert_eq!(compiles, 1, "a matching dump skips kotlinc");

        let changed = fingerprint_parts(&[b"source-v2"]);
        let third = cached_at(query(changed), || compile(&mut compiles));
        assert_eq!(third.unwrap(), vec![b"class-a".to_vec()]);
        assert_eq!(compiles, 2, "a new fingerprint recompiles");

        let mut snapshot_query = query(changed);
        snapshot_query.build_id = None;
        let snapshot = cached_at(snapshot_query, || compile(&mut compiles));
        assert!(snapshot.is_some());
        assert_eq!(compiles, 3);
        let mut again_query = query(changed);
        again_query.build_id = None;
        let again = cached_at(again_query, || compile(&mut compiles));
        assert!(again.is_some());
        assert_eq!(compiles, 4, "a snapshot never reuses a dump");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn ci_does_not_write_a_missing_dump() {
        let root = std::env::temp_dir().join(format!(
            "krusty-byte-dump-ci-{}-{}",
            std::process::id(),
            fingerprint_parts(&[b"byte-dump-ci"])
        ));
        let _ = std::fs::remove_dir_all(&root);
        let fingerprint = fingerprint_parts(&[b"source"]);
        let classes = ["A"];
        let got = cached_at(
            CacheQuery {
                root: &root,
                build_id: Some("2.4.20-release-1"),
                module: "mod",
                case: "case",
                stem: "Stem",
                jvm_target: "default",
                variant: "",
                fingerprint,
                classes: &classes,
                force: false,
                write: false,
            },
            || {
                let mut map = BTreeMap::new();
                map.insert("A".to_string(), b"bytes".to_vec());
                Some(map)
            },
        );
        assert_eq!(got.unwrap(), vec![b"bytes".to_vec()]);
        assert!(
            !root.exists(),
            "a miss that is not allowed to record leaves no dump"
        );
    }

    #[test]
    fn a_directory_classpath_ignores_its_own_name() {
        let root = std::env::temp_dir().join(format!(
            "krusty-byte-dump-cp-{}-{}",
            std::process::id(),
            fingerprint_parts(&[b"cp-name"])
        ));
        let _ = std::fs::remove_dir_all(&root);
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
    fn a_class_tree_is_reused_until_its_fingerprint_changes() {
        let root = std::env::temp_dir().join(format!(
            "krusty-byte-dump-tree-{}-{}",
            std::process::id(),
            fingerprint_parts(&[b"tree"])
        ));
        let _ = std::fs::remove_dir_all(&root);
        let dir = root.join("tree");
        let fingerprint = fingerprint_parts(&[b"tree-source"]);
        let mut compiles = 0u32;
        let compile = |compiles: &mut u32, bytes: &[u8]| {
            *compiles += 1;
            let mut map = BTreeMap::new();
            map.insert("pkg/A.class".to_string(), bytes.to_vec());
            map.insert(
                "META-INF/main.kotlin_module".to_string(),
                b"module".to_vec(),
            );
            Some(map)
        };
        let first = cached_tree(&dir, fingerprint, false, true, || {
            compile(&mut compiles, b"one")
        });
        assert_eq!(first.unwrap().get("pkg/A.class").unwrap(), b"one");
        assert_eq!(compiles, 1);
        let second = cached_tree(&dir, fingerprint, false, true, || {
            compile(&mut compiles, b"two")
        });
        assert_eq!(second.unwrap().get("pkg/A.class").unwrap(), b"one");
        assert_eq!(compiles, 1, "a matching tree skips kotlinc");
        let changed = fingerprint_parts(&[b"tree-source-v2"]);
        let third = cached_tree(&dir, changed, false, true, || {
            compile(&mut compiles, b"two")
        });
        assert_eq!(third.unwrap().get("pkg/A.class").unwrap(), b"two");
        assert_eq!(compiles, 2, "a new fingerprint recompiles");
        let _ = std::fs::remove_dir_all(&root);
    }
}
