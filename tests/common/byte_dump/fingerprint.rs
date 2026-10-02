//! Location-independent identities for every input that can change kotlinc's class output.
//!
//! Scratch inputs are hashed directly. Selected toolchain entries retain their logical role/path,
//! but compact that installation's immutable bytes into one process-cached content identity so a
//! fixture never mistakes a patched compiler or JDK image for another build.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use super::hex128;

#[path = "fingerprint/sha256.rs"]
mod sha256;
use sha256::Sha256;

#[cfg(test)]
static SELECTED_CONTENT_HASHES: OnceLock<Mutex<HashMap<PathBuf, usize>>> = OnceLock::new();

#[cfg(test)]
pub(super) fn selected_content_hash_count(path: &Path) -> usize {
    let Ok(path) = path.canonicalize() else {
        return 0;
    };
    SELECTED_CONTENT_HASHES
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .expect("selected toolchain test counters")
        .get(&path)
        .copied()
        .unwrap_or(0)
}

#[cfg(test)]
fn record_selected_content_hash(path: &Path) {
    *SELECTED_CONTENT_HASHES
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .expect("selected toolchain test counters")
        .entry(path.to_path_buf())
        .or_default() += 1;
}

#[cfg(not(test))]
fn record_selected_content_hash(_: &Path) {}

/// Inputs that decide which dump a fixture uses, with no absolute paths in either field.
///
/// The fingerprint covers the source, the JVM target, the normalized flags, the ambient selected
/// Kotlin/JDK toolchain content, and dependency class files supplied to the fixture. A scratch
/// classpath directory contributes those file bytes and not the directory's name, so the next run
/// still hits and a changed class file misses. Selected Kotlin-distribution and JDK entries
/// contribute compact, location-independent content identities. File-valued options contribute
/// bytes rather than scratch paths.
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
    let kotlinc_lib = krusty::toolchain::kotlinc_lib_dir();
    let jdk_modules = krusty::toolchain::jdk_modules();
    class_dump_inputs_with_platform(
        source,
        jvm_target,
        extra_args,
        classpath,
        kotlinc_lib.as_deref(),
        jdk_modules.as_deref(),
    )
}

pub(super) fn class_dump_inputs_with_platform(
    source: &str,
    jvm_target: &str,
    extra_args: &[String],
    classpath: &[PathBuf],
    kotlinc_lib: Option<&Path>,
    jdk_modules: Option<&Path>,
) -> ClassDumpInputs {
    let variant = match normalize_args(extra_args) {
        Ok(variant) => variant,
        Err(err) => super::refuse_missing_dump(&err),
    };
    let classpath =
        match classpath_content_fingerprint_with_platform(classpath, kotlinc_lib, jdk_modules) {
            Ok(classpath) => classpath,
            Err(err) => super::refuse_missing_dump(&err),
        };
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

pub(super) fn normalize_args(args: &[String]) -> Result<String, String> {
    args.iter()
        .map(|arg| normalize_invocation_flag(arg))
        .collect::<Result<Vec<_>, _>>()
        .map(|args| args.join("\n"))
}

pub(super) fn basename(value: &str) -> String {
    Path::new(value)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(value)
        .to_string()
}

pub(super) fn hash_tree(root: &Path) -> Result<u64, String> {
    let mut files = Vec::new();
    collect_files(root, &mut files)?;
    files.sort();
    let mut hash = 0xcbf29ce484222325u64;
    for path in files {
        if let Ok(relative) = path.strip_prefix(root) {
            hash = fnv64(
                hash,
                relative.to_string_lossy().replace('\\', "/").as_bytes(),
            );
        }
        let bytes = std::fs::read(&path)
            .map_err(|err| format!("unreadable file {}: {err}", path.display()))?;
        hash = fnv64(hash, &bytes);
    }
    Ok(hash)
}

fn collect_files(root: &Path, files: &mut Vec<PathBuf>) -> Result<(), String> {
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir)
            .map_err(|err| format!("unreadable directory {}: {err}", dir.display()))?;
        for entry in entries {
            let entry =
                entry.map_err(|err| format!("unreadable directory {}: {err}", dir.display()))?;
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.is_file() {
                files.push(path);
            }
        }
    }
    Ok(())
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

pub(super) fn fnv64(mut hash: u64, bytes: &[u8]) -> u64 {
    for byte in bytes {
        hash = (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3);
    }
    hash
}

pub(super) fn normalize_invocation_flag(arg: &str) -> Result<String, String> {
    if let Some(paths) = arg.strip_prefix("-Xplugin=") {
        let mut hashed = Vec::new();
        for path in paths.split(',') {
            let bytes =
                std::fs::read(path).map_err(|err| format!("unreadable plugin {path}: {err}"))?;
            hashed.push(hex128(fingerprint_parts(&[&bytes])));
        }
        return Ok(format!("-Xplugin={}", hashed.join(",")));
    }
    // A flag whose value is a scratch path (`-Xcommon-sources=/tmp/.../A.kt`) names the same
    // inputs wherever the files were written. Hash those bytes and keep every other flag literal.
    let Some((name, value)) = arg.split_once('=') else {
        return Ok(arg.to_string());
    };
    if !matches!(name, "-Xcommon-sources" | "-Xfriend-paths") {
        return Ok(arg.to_string());
    }
    let mut hashed = Vec::new();
    for part in value.split(',') {
        let bytes =
            std::fs::read(part).map_err(|err| format!("unreadable flag file {part}: {err}"))?;
        hashed.push(hex128(fingerprint_parts(&[&bytes])));
    }
    Ok(format!("{name}={}", hashed.join(",")))
}

/// Ambient selected toolchains followed by the explicit classpath entries.
///
/// Every lookup starts with the selected kotlinc `lib/` tree and JDK `modules` content identities,
/// because kotlinc loads both even when the explicit classpath is empty. A proven entry under the
/// selected kotlinc `lib/` additionally contributes its logical distribution path. Explicit
/// selected JDK `modules` and `ct.sym` entries contribute their kind, exact image content, and the
/// small `release` label. Every arbitrary dependency—including a jar with a stdlib-like basename
/// or a different `lib/modules`—contributes its own content, in declaration order.
pub(super) fn classpath_content_fingerprint(paths: &[PathBuf]) -> Result<String, String> {
    let kotlinc_lib = krusty::toolchain::kotlinc_lib_dir();
    let jdk_modules = krusty::toolchain::jdk_modules();
    classpath_content_fingerprint_with_platform(
        paths,
        kotlinc_lib.as_deref(),
        jdk_modules.as_deref(),
    )
}

pub(super) fn classpath_content_fingerprint_with_platform(
    paths: &[PathBuf],
    kotlinc_lib: Option<&Path>,
    jdk_modules: Option<&Path>,
) -> Result<String, String> {
    let selected_kotlinc = selected_kotlinc_root(kotlinc_lib)?;
    let mut rows = Vec::new();
    let kotlinc_identity = selected_kotlinc
        .as_deref()
        .map(selected_content_identity)
        .transpose()?;
    if let Some(identity) = kotlinc_identity.as_deref() {
        rows.push(format!("toolchain:kotlinc:{identity}"));
    }
    if let Some(row) = selected_jdk_identity(jdk_modules)? {
        rows.push(row);
    }
    for path in paths {
        if let Some(relative) = selected_kotlinc_entry(path, selected_kotlinc.as_deref()) {
            let identity = kotlinc_identity
                .as_deref()
                .expect("a selected entry has its selected distribution identity");
            rows.push(format!("kotlinc:{relative}:{identity}"));
            continue;
        }
        if let Some(row) = selected_jdk_entry(path, jdk_modules)? {
            rows.push(row);
            continue;
        }
        if path.is_dir() {
            rows.push(format!("dir:{:016x}", hash_tree(path)?));
        } else {
            let bytes = std::fs::read(path)
                .map_err(|err| format!("unreadable classpath entry {}: {err}", path.display()))?;
            rows.push(format!("file:{:032x}", fingerprint_parts(&[&bytes])));
        }
    }
    Ok(rows.join("\n"))
}

fn selected_kotlinc_root(selected_lib: Option<&Path>) -> Result<Option<PathBuf>, String> {
    selected_lib
        .map(|path| {
            path.canonicalize().map_err(|err| {
                format!(
                    "unreadable selected toolchain input {}: {err}",
                    path.display()
                )
            })
        })
        .transpose()
}

fn selected_kotlinc_entry(path: &Path, selected_lib: Option<&Path>) -> Option<String> {
    let selected_lib = selected_lib?;
    let path = path.canonicalize().ok()?;
    let relative = path.strip_prefix(selected_lib).ok()?;
    let relative = relative.to_string_lossy().replace('\\', "/");
    Some(if relative.is_empty() {
        ".".to_string()
    } else {
        relative
    })
}

fn selected_jdk_entry(
    path: &Path,
    selected_modules: Option<&Path>,
) -> Result<Option<String>, String> {
    let Some(selected_modules) = selected_modules else {
        return Ok(None);
    };
    let Ok(selected_modules) = selected_modules.canonicalize() else {
        return Ok(None);
    };
    let Ok(path) = path.canonicalize() else {
        return Ok(None);
    };
    let Some(lib) = selected_modules.parent() else {
        return Ok(None);
    };
    let kind = if path == selected_modules {
        "modules"
    } else if lib.join("ct.sym").canonicalize().ok().as_ref() == Some(&path) {
        "ct.sym"
    } else {
        return Ok(None);
    };
    let Some(home) = lib.parent() else {
        return Ok(None);
    };
    let release = home.join("release");
    let release = std::fs::read(&release).map_err(|err| {
        format!(
            "unreadable selected JDK identity {}: {err}",
            release.display()
        )
    })?;
    let content = selected_content_identity(&path)?;
    Ok(Some(format!(
        "jdk:{kind}:{:032x}:{content}",
        fingerprint_parts(&[&release])
    )))
}

fn selected_jdk_identity(selected_modules: Option<&Path>) -> Result<Option<String>, String> {
    let Some(selected_modules) = selected_modules else {
        return Ok(None);
    };
    let selected_modules = selected_modules.canonicalize().map_err(|err| {
        format!(
            "unreadable selected toolchain input {}: {err}",
            selected_modules.display()
        )
    })?;
    let Some(lib) = selected_modules.parent() else {
        return Err(format!(
            "selected JDK modules input has no lib directory: {}",
            selected_modules.display()
        ));
    };
    let Some(home) = lib.parent() else {
        return Err(format!(
            "selected JDK modules input has no home directory: {}",
            selected_modules.display()
        ));
    };
    let release = home.join("release");
    let release = std::fs::read(&release).map_err(|err| {
        format!(
            "unreadable selected JDK identity {}: {err}",
            release.display()
        )
    })?;
    let content = selected_content_identity(&selected_modules)?;
    Ok(Some(format!(
        "toolchain:jdk:modules:{:032x}:{content}",
        fingerprint_parts(&[&release])
    )))
}

/// Hash an immutable selected installation artifact once in this process.
///
/// The cache key is the canonical artifact identity, but the published digest contains no path. A
/// second installation with the same bytes therefore produces the same class-dump key; a patched
/// installation produces a hard miss. Selected toolchains are immutable for one test process.
fn selected_content_identity(path: &Path) -> Result<String, String> {
    static IDENTITIES: OnceLock<Mutex<HashMap<PathBuf, String>>> = OnceLock::new();
    let canonical = path.canonicalize().map_err(|err| {
        format!(
            "unreadable selected toolchain input {}: {err}",
            path.display()
        )
    })?;
    let identities = IDENTITIES.get_or_init(|| Mutex::new(HashMap::new()));
    let mut identities = identities
        .lock()
        .expect("selected toolchain identity cache");
    if let Some(identity) = identities.get(&canonical).cloned() {
        return Ok(identity);
    }
    record_selected_content_hash(&canonical);
    let digest = if canonical.is_dir() {
        fingerprint_tree(&canonical)?
    } else {
        fingerprint_file(&canonical)?
    };
    let identity = hex_sha256(&digest);
    identities.insert(canonical, identity.clone());
    Ok(identity)
}

fn fingerprint_tree(root: &Path) -> Result<[u8; 32], String> {
    let mut files = Vec::new();
    collect_files(root, &mut files)?;
    files.sort();
    let mut hash = Sha256::default();
    hash.update(&(files.len() as u64).to_le_bytes());
    for path in files {
        let relative = path
            .strip_prefix(root)
            .expect("a collected file remains below its root")
            .to_string_lossy()
            .replace('\\', "/");
        let digest = fingerprint_file(&path)?;
        hash.update(&(relative.len() as u64).to_le_bytes());
        hash.update(relative.as_bytes());
        hash.update(&digest);
    }
    Ok(hash.finalize())
}

fn fingerprint_file(path: &Path) -> Result<[u8; 32], String> {
    let mut file = std::fs::File::open(path).map_err(|err| {
        format!(
            "unreadable selected toolchain input {}: {err}",
            path.display()
        )
    })?;
    let mut hash = Sha256::default();
    let mut buffer = [0u8; 1024 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(|err| {
            format!(
                "unreadable selected toolchain input {}: {err}",
                path.display()
            )
        })?;
        if read == 0 {
            break;
        }
        hash.update(&buffer[..read]);
    }
    Ok(hash.finalize())
}

fn hex_sha256(bytes: &[u8; 32]) -> String {
    let mut value = String::with_capacity(64);
    for byte in bytes {
        use std::fmt::Write;
        write!(value, "{byte:02x}").expect("writing to a String cannot fail");
    }
    value
}
