//! Dependency fixtures built by the reference kotlinc.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::byte_dump::{self, fingerprint_parts};
use super::{kotlinc_compile, language_directives, scratch_dir, stdlib_jar};

/// Compile a dependency source set with the REFERENCE kotlinc (pooled server) into a scratch
/// classpath dir. `None` = toolchain unavailable; kotlinc REJECTING the sources panics — the
/// fixture is invalid Kotlin, which must never read as a skip. Each source's `// LANGUAGE:`
/// directives reach kotlinc as the `-XXLanguage:` flags krusty reads from the same directives.
///
/// A release or RC compiler reuses a dump of that output, including `.kotlin_module` files, so a
/// later run does not invoke kotlinc again. A snapshot, dev, or beta compiler always compiles.
pub fn kotlinc_lib_out(sources: &[(&str, &str)]) -> Option<PathBuf> {
    kotlinc_lib_out_with(sources, &[])
}

/// [`kotlinc_lib_out`] with additional public kotlinc options. The options are part of the cache
/// identity: a dependency compiled at one language/API level must never stand in for another.
pub(crate) fn kotlinc_lib_out_with(
    sources: &[(&str, &str)],
    kotlinc_extra: &[String],
) -> Option<PathBuf> {
    let fingerprint = lib_fingerprint(sources, kotlinc_extra);
    let slot = format!("{fingerprint:032x}");
    if let Some(files) = byte_dump::load_shared_files(&slot, fingerprint) {
        return materialize(&files);
    }
    let out = compile_lib(sources, kotlinc_extra)?;
    if let Some(files) = read_output(&out) {
        byte_dump::store_shared_files(&slot, fingerprint, &files);
    }
    Some(out)
}

fn lib_fingerprint(sources: &[(&str, &str)], kotlinc_extra: &[String]) -> u128 {
    let mut blob = Vec::new();
    for (name, src) in sources {
        blob.extend_from_slice(&(name.len() as u64).to_le_bytes());
        blob.extend_from_slice(name.as_bytes());
        blob.extend_from_slice(&(src.len() as u64).to_le_bytes());
        blob.extend_from_slice(src.as_bytes());
    }
    for argument in kotlinc_extra {
        blob.extend_from_slice(&(argument.len() as u64).to_le_bytes());
        blob.extend_from_slice(argument.as_bytes());
    }
    fingerprint_parts(&[&blob])
}

fn compile_lib(sources: &[(&str, &str)], kotlinc_extra: &[String]) -> Option<PathBuf> {
    let stdlib = stdlib_jar();
    let work = scratch_dir()?;
    let out = work.join("libout");
    std::fs::create_dir_all(&out).ok()?;
    let mut args = vec![
        "-d".into(),
        out.to_string_lossy().into_owned(),
        "-cp".into(),
        stdlib.to_string_lossy().into_owned(),
    ];
    let mut language = Vec::new();
    for (name, src) in sources {
        let path = work.join(name);
        std::fs::write(&path, src).ok()?;
        args.push(path.to_string_lossy().into_owned());
        for flag in language_directives::kotlinc_args(src) {
            if !language.contains(&flag) {
                language.push(flag);
            }
        }
    }
    args.extend(language);
    args.extend(kotlinc_extra.iter().cloned());
    match kotlinc_compile(&args) {
        Some((0, _)) => Some(out),
        Some((code, err)) => panic!("kotlinc(lib) failed ({code}): {err}"),
        None => None,
    }
}

fn materialize(files: &BTreeMap<String, Vec<u8>>) -> Option<PathBuf> {
    let out = scratch_dir()?.join("libout");
    std::fs::create_dir_all(&out).ok()?;
    for (relative, bytes) in files {
        let path = out.join(relative);
        std::fs::create_dir_all(path.parent()?).ok()?;
        std::fs::write(&path, bytes).ok()?;
    }
    Some(out)
}

fn read_output(root: &Path) -> Option<BTreeMap<String, Vec<u8>>> {
    let mut files = BTreeMap::new();
    read_output_in(root, root, &mut files)?;
    Some(files)
}

fn read_output_in(root: &Path, dir: &Path, files: &mut BTreeMap<String, Vec<u8>>) -> Option<()> {
    for entry in std::fs::read_dir(dir).ok()? {
        let path = entry.ok()?.path();
        if path.is_dir() {
            read_output_in(root, &path, files)?;
        } else if path.is_file() {
            let relative = path.strip_prefix(root).ok()?;
            let name = relative.to_string_lossy().replace('\\', "/");
            files.insert(name, std::fs::read(&path).ok()?);
        }
    }
    Some(())
}
