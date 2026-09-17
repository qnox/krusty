//! Byte equality for KLIBs: build the same module with krusty and with the reference compiler, then
//! compare the two artifacts entry by entry.
//!
//! A `.klib` is a distributed artifact, so "krusty writes a klib the reader accepts" is not the bar.
//! A dependent resolves against its `unique_name`, a linker walks its fragments, and a fingerprint
//! over its bytes decides whether a downstream module recompiles — every one of which reads bytes
//! kotlinc wrote. The bar is therefore the same one the JVM lane already holds itself to, and the
//! instrument is the same shape: [`KRUSTY_LIB_BYTEDIFF_REPORT`](super)'s `LIBDIFF` lines have a
//! `KLIBDIFF` counterpart here.
//!
//! The METADATA-only klib is the comparison target, and the DIRECTORY shape is what both sides
//! write. `K2MetadataCompiler -Xmetadata-klib` produces a directory with no `default/ir` at all, so
//! a difference is a difference in declarations rather than in zip container metadata that no
//! consumer reads for meaning.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::common;

/// Per-entry verdicts comparing a krusty-written klib against a reference-written one.
#[derive(Debug, Default)]
pub struct KlibEntryDiff {
    /// Entries present in both, with identical bytes.
    pub identical: Vec<String>,
    /// Present in both and different, with each side's length.
    pub divergent: Vec<(String, usize, usize)>,
    /// Written by the reference compiler and not by krusty — the work not yet done.
    pub missing: Vec<String>,
    /// Written by krusty and not by the reference compiler — invented output, always a defect.
    pub extra: Vec<String>,
}

impl KlibEntryDiff {
    /// Every entry the reference compiler wrote, however krusty fared on it.
    pub fn reference_entries(&self) -> usize {
        self.identical.len() + self.divergent.len() + self.missing.len()
    }

    /// A single line per entry on stderr, under the same opt-in the JVM lane uses. The prefix is
    /// grep-able so a run's whole ledger can be read off a log.
    pub fn report(&self, tag: &str) {
        if std::env::var("KRUSTY_KLIB_BYTEDIFF_REPORT").is_err() {
            return;
        }
        for entry in &self.identical {
            eprintln!("KLIBDIFF\tidentical\t{entry}\t{tag}");
        }
        for (entry, krusty, reference) in &self.divergent {
            eprintln!("KLIBDIFF\tdivergent\t{entry}\t{tag}\tkrusty={krusty}\tref={reference}");
        }
        for entry in &self.missing {
            eprintln!("KLIBDIFF\tmissing\t{entry}\t{tag}");
        }
        for entry in &self.extra {
            eprintln!("KLIBDIFF\textra\t{entry}\t{tag}");
        }
        eprintln!(
            "KLIBDIFF\tsummary\t{tag}\tidentical={}\tdivergent={}\tmissing={}\textra={}",
            self.identical.len(),
            self.divergent.len(),
            self.missing.len(),
            self.extra.len()
        );
    }
}

/// Compare two klib DIRECTORIES entry by entry.
pub fn diff_klib_directories(krusty: &Path, reference: &Path) -> KlibEntryDiff {
    let ours = read_tree(krusty);
    let theirs = read_tree(reference);
    let mut diff = KlibEntryDiff::default();
    for (entry, reference_bytes) in &theirs {
        match ours.get(entry) {
            None => diff.missing.push(entry.clone()),
            Some(our_bytes) if our_bytes == reference_bytes => diff.identical.push(entry.clone()),
            Some(our_bytes) => {
                diff.divergent
                    .push((entry.clone(), our_bytes.len(), reference_bytes.len()))
            }
        }
    }
    for entry in ours.keys() {
        if !theirs.contains_key(entry) {
            diff.extra.push(entry.clone());
        }
    }
    diff
}

fn read_tree(root: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    collect(root, root, &mut out);
    out
}

fn collect(root: &Path, current: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
    let Ok(directory) = std::fs::read_dir(current) else {
        return;
    };
    for child in directory.flatten() {
        let path = child.path();
        if path.is_dir() {
            collect(root, &path, out);
        } else if let (Ok(relative), Ok(bytes)) = (path.strip_prefix(root), std::fs::read(&path)) {
            out.insert(relative.to_string_lossy().replace('\\', "/"), bytes);
        }
    }
}

/// Compile `sources` into a METADATA-only klib with the reference compiler, as a directory.
///
/// This is `K2MetadataCompiler`, not the JVM or JS one: it is the compiler whose klib carries
/// declarations and nothing else, which makes it the target krusty's own writer can be held to
/// before any IR serialization exists.
///
/// `None` = the toolchain is unavailable. kotlinc REJECTING the sources panics — an invalid fixture
/// must never read as a skip.
pub fn kotlinc_metadata_klib(tag: &str, sources: &[(&str, &str)]) -> Option<PathBuf> {
    let stdlib = common::stdlib_jar();
    if !stdlib.is_file() {
        return None;
    }
    let work = common::scratch_dir()?.join(format!("metadata-klib-{tag}"));
    let out = work.join("out");
    std::fs::create_dir_all(&out).ok()?;
    let mut args = vec![
        "-Xmetadata-klib".to_string(),
        "-classpath".to_string(),
        stdlib.to_string_lossy().into_owned(),
        "-d".to_string(),
        out.to_string_lossy().into_owned(),
    ];
    for (name, source) in sources {
        let path = work.join(name);
        std::fs::write(&path, source).ok()?;
        args.push(path.to_string_lossy().into_owned());
    }
    match common::kotlinc_compile_with(
        "org.jetbrains.kotlin.cli.metadata.K2MetadataCompiler",
        &args,
    ) {
        Some((0, _)) => Some(out),
        Some((code, err)) => panic!("kotlinc(metadata-klib) failed ({code}): {err}"),
        None => None,
    }
}
