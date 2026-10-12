//! Shared storage and toolchain discovery for recorded reference-compiler observations.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use krusty::compilation_target::CompilationTarget;

use super::common::byte_dump::{self, fingerprint_parts};

/// Read the recording for this source set, or produce and record it with `live`.
pub(crate) fn recorded(
    target: CompilationTarget,
    kind: &str,
    sources: &[(&str, &str)],
    args: &[String],
    live: impl FnOnce(&Path) -> Option<BTreeMap<String, Vec<u8>>>,
) -> Option<BTreeMap<String, Vec<u8>>> {
    let fingerprint = fingerprint(target, kind, sources, args);
    let slot = format!("reference-{kind}-{target:?}-{fingerprint:032x}");
    if let Some(files) = byte_dump::load_shared_files(&slot, fingerprint) {
        return Some(files);
    }
    let work = super::common::scratch_dir().expect("cannot allocate a reference-compiler fixture");
    let answer = live(&work);
    let _ = std::fs::remove_dir_all(&work);
    let answer = answer?;
    byte_dump::store_shared_files(&slot, fingerprint, &answer);
    Some(answer)
}

fn fingerprint(
    target: CompilationTarget,
    kind: &str,
    sources: &[(&str, &str)],
    args: &[String],
) -> u128 {
    let mut blob = Vec::new();
    let mut field = |bytes: &[u8]| {
        blob.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
        blob.extend_from_slice(bytes);
    };
    field(format!("{target:?}").as_bytes());
    field(kind.as_bytes());
    for (name, text) in sources {
        field(name.as_bytes());
        field(text.as_bytes());
    }
    for argument in args {
        field(argument.as_bytes());
    }
    fingerprint_parts(&[&blob])
}

/// The Kotlin/Native distribution root, when provisioned.
pub(crate) fn kotlin_native_root() -> Option<PathBuf> {
    let root = std::env::var_os("KRUSTY_KOTLIN_NATIVE")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    if root.is_none() && std::env::var_os("KRUSTY_REQUIRE_KOTLIN_NATIVE").is_some() {
        panic!("KRUSTY_REQUIRE_KOTLIN_NATIVE is set but KRUSTY_KOTLIN_NATIVE is not");
    }
    root
}
