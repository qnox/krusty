//! One atomic, independently compressed reference inventory per exact-input fingerprint.
//!
//! Corpus entries never rebuild an archive containing earlier cases. This keeps cold cache writes
//! linear in the emitted bytes, like the JVM reference-class directory cache.
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

const FORMAT: &[u8] = b"box-klib-entries-v1";

pub(super) fn path(fingerprint: u128) -> PathBuf {
    std::env::var_os("KRUSTY_KLIB_REFERENCE_CACHE")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR")).join("target/cache/klib-references")
        })
        .join(format!("{fingerprint:032x}.zip"))
}

pub(super) fn load(path: &Path, fingerprint: u128) -> Option<BTreeMap<String, Vec<u8>>> {
    let file = std::fs::File::open(path).ok()?;
    let mut archive = zip::ZipArchive::new(file).ok()?;
    let mut entries = BTreeMap::new();
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).ok()?;
        let name = entry.name().ok()?.into_owned();
        if entry.is_dir() || !safe(&name) {
            return None;
        }
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).ok()?;
        if entries.insert(name, bytes).is_some() {
            return None;
        }
    }
    if entries.remove("@format")?.as_slice() != FORMAT
        || entries.remove("@fingerprint")?.as_slice() != fingerprint.to_be_bytes()
    {
        return None;
    }
    (!entries.is_empty()).then_some(entries)
}

pub(super) fn store(
    path: &Path,
    fingerprint: u128,
    entries: &BTreeMap<String, Vec<u8>>,
) -> Result<(), String> {
    if entries.is_empty() || entries.contains_key("@format") || entries.contains_key("@fingerprint")
    {
        return Err("invalid reference cache payload".into());
    }
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let parent = path.parent().ok_or("reference cache has no parent")?;
    std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let staging = parent.join(format!(
        ".{fingerprint:032x}-{}-{}.tmp",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let write = || -> Result<(), String> {
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&staging)
            .map_err(|error| error.to_string())?;
        let mut archive = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for (name, bytes) in [
            ("@format", FORMAT),
            ("@fingerprint", fingerprint.to_be_bytes().as_slice()),
        ]
        .into_iter()
        .chain(
            entries
                .iter()
                .map(|(name, bytes)| (name.as_str(), bytes.as_slice())),
        ) {
            if !safe(name) {
                return Err(format!("unsafe reference cache entry {name:?}"));
            }
            archive
                .start_file(name, options)
                .map_err(|error| error.to_string())?;
            archive
                .write_all(bytes)
                .map_err(|error| error.to_string())?;
        }
        archive.finish().map_err(|error| error.to_string())?;
        std::fs::rename(&staging, path).map_err(|error| error.to_string())
    };
    let result = write();
    if result.is_err() {
        let _ = std::fs::remove_file(staging);
    }
    result
}

fn safe(name: &str) -> bool {
    !name.is_empty()
        && !Path::new(name).is_absolute()
        && Path::new(name)
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
}
