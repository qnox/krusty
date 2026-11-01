//! What a dependent compilation observes of a compiled class directory, and its fingerprint.
//!
//! A build tool that recompiles a module only when this fingerprint of a dependency's output
//! moves skips the dependents of a body-only edit, and still recompiles them for every change
//! they could observe. The fingerprint is conservative rather than minimal:
//!
//! * **A class whose bytecode reaches a dependent contributes all of its bytes.** That is a class
//!   declaring an `inline` function or accessor (the inliner splices its bodies into the call
//!   site, and their `$default` variants live beside them), an anonymous class and a synthetic
//!   class such as a SAM wrapper (an inline call regenerates them into the caller).
//! * **Every other class contributes its compiler-visible declaration model**, the
//!   [`ClassInfo`](krusty::jvm::classreader::ClassInfo) the class reader hands the frontend:
//!   signatures, `ConstantValue`s (folded into the consumer), annotations with their payloads, and
//!   the fully decoded `@kotlin.Metadata`, contracts included. It carries no method body, so a body
//!   edit leaves it unchanged. Private members are kept: changing one recompiles dependents for
//!   nothing, which costs time but never correctness. A field the class reader gains later joins
//!   the fingerprint without a change here.
//! * **Any other file contributes its bytes**, as does a class file the reader rejects.
//!
//! Unlike [`crate::abi`], which models a reduced ABI with no bodies at all, this view is complete
//! for krusty as the dependent's compiler, so a dependent may be keyed on it.

use std::path::{Path, PathBuf};

use krusty::jvm::classreader::{parse_class, ClassInfo, ACC_SYNTHETIC};

use crate::digest::{digest_bytes, Digest, Hasher};

/// The fingerprint of every file under `root`, keyed by its `/`-separated path relative to `root`.
pub fn directory_fingerprint(root: &Path) -> std::io::Result<Digest> {
    let mut files = Vec::new();
    collect_files(root, String::new(), &mut files)?;
    files.sort();
    let views = in_parallel(&files, |(relative, path)| {
        std::fs::read(path).map(|bytes| file_view(relative, &bytes))
    })?;
    Ok(fingerprint_of(
        files.iter().map(|(relative, _)| relative.as_str()),
        views,
    ))
}

/// The fingerprint of every file in the archive at `path`, keyed by its entry name and read
/// through the same view as a file of [`directory_fingerprint`]'s, so a dependency that reaches
/// its dependents as a jar is keyed as it is from its class directory. A directory entry holds no
/// file and enters as nothing, as a directory does; the archive's own layout (entry order,
/// timestamps, compression) is no part of what the compilation reads and none of the fingerprint.
pub fn archive_fingerprint(path: &Path) -> std::io::Result<Digest> {
    let invalid = |error| std::io::Error::new(std::io::ErrorKind::InvalidData, error);
    let mut archive = krusty::archive::open_file(std::fs::File::open(path)?).map_err(invalid)?;
    let mut files = Vec::new();
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(invalid)?;
        if entry.is_dir() {
            continue;
        }
        let name = entry.name().map_err(invalid)?.into_owned();
        let mut bytes = Vec::with_capacity(entry.size() as usize);
        std::io::Read::read_to_end(&mut entry, &mut bytes)?;
        files.push((name, bytes));
    }
    files.sort_by(|(left, _), (right, _)| left.cmp(right));
    let views = in_parallel(&files, |(name, bytes)| Ok(file_view(name, bytes)))?;
    Ok(fingerprint_of(
        files.iter().map(|(name, _)| name.as_str()),
        views,
    ))
}

/// `view` of every item, in order. A generated client is tens of thousands of classes; each file's
/// view is independent.
fn in_parallel<T: Sync>(
    items: &[T],
    view: impl Fn(&T) -> std::io::Result<(&'static str, Digest)> + Sync,
) -> std::io::Result<Vec<(&'static str, Digest)>> {
    let workers = std::thread::available_parallelism().map_or(1, usize::from);
    let chunk = items.len().div_ceil(workers).max(1);
    let view = &view;
    let views = std::thread::scope(|scope| {
        let handles: Vec<_> = items
            .chunks(chunk)
            .map(|chunk| {
                scope.spawn(move || chunk.iter().map(view).collect::<std::io::Result<Vec<_>>>())
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().expect("a fingerprint worker panicked"))
            .collect::<std::io::Result<Vec<_>>>()
    })?;
    Ok(views.into_iter().flatten().collect())
}

/// The fingerprint of the files named `paths`, in that order, entering by `views`.
fn fingerprint_of<'a>(
    paths: impl ExactSizeIterator<Item = &'a str>,
    views: Vec<(&'static str, Digest)>,
) -> Digest {
    let mut hasher = Hasher::new();
    hasher.count("files", paths.len());
    for (relative, (view, digest)) in paths.zip(views) {
        hasher.text("path", relative);
        hasher.text("view", view);
        hasher.nested("digest", digest);
    }
    hasher.finish()
}

fn collect_files(
    directory: &Path,
    prefix: String,
    files: &mut Vec<(String, PathBuf)>,
) -> std::io::Result<()> {
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let relative = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };
        if entry.file_type()?.is_dir() {
            collect_files(&entry.path(), relative, files)?;
        } else {
            files.push((relative, entry.path()));
        }
    }
    Ok(())
}

/// How one file enters the fingerprint: its declaration model or all of its bytes.
fn file_view(relative: &str, bytes: &[u8]) -> (&'static str, Digest) {
    if relative.ends_with(".class") {
        if let Ok(class) = parse_class(bytes) {
            if !bytecode_reaches_dependents(&class) {
                return (
                    "declarations",
                    digest_bytes(format!("{class:?}").as_bytes()),
                );
            }
        }
    }
    ("bytes", digest_bytes(bytes))
}

fn bytecode_reaches_dependents(class: &ClassInfo) -> bool {
    let meta = &class.meta;
    class.access & ACC_SYNTHETIC != 0
        || class
            .inner_class_self()
            .is_some_and(|entry| entry.name.is_none())
        || meta
            .class_functions
            .iter()
            .chain(meta.package_functions.iter())
            .any(|function| function.is_inline())
        || meta
            .class_properties
            .iter()
            .chain(meta.package_properties.iter())
            .any(|property| property.getter_inline || property.setter_inline)
}
