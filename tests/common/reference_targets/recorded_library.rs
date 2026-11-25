//! The recorded reference library: a KLIB directory a kotlinc-js/kotlinc-wasm run produces is kept
//! under the exact compiler identity like every other reference observation, so a later run replays
//! the status, the diagnostics and the library bytes instead of starting the compiler again.

use krusty::compilation_target::CompilationTarget;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[path = "invocation.rs"]
mod live;
use live::{run_live, Reference};

/// Run the reference compiler for `target` over `paths`, leaving a library in
/// `work/reference-out`, and record or replay the KLIB a kotlinc-js/kotlinc-wasm run produces.
pub fn run_reference(
    target: CompilationTarget,
    work: &Path,
    paths: &[PathBuf],
    args: &[String],
    program: bool,
) -> Option<(Reference, PathBuf)> {
    let out = work.join("reference-out");
    let records_library = !program
        && matches!(
            target,
            CompilationTarget::Js | CompilationTarget::WasmJs | CompilationTarget::WasmWasi
        );
    if !records_library {
        return run_live(target, work, paths, args, program);
    }
    let sources = paths
        .iter()
        .map(|path| {
            (
                // The file name, not the scratch directory it sits in, is part of the identity.
                path.file_name()
                    .expect("a reference source is a file")
                    .to_string_lossy()
                    .into_owned(),
                std::fs::read_to_string(path).expect("read a reference source"),
            )
        })
        .collect::<Vec<_>>();
    let sources = sources
        .iter()
        .map(|(name, text)| (name.as_str(), text.as_str()))
        .collect::<Vec<_>>();
    let files = crate::reference_recording_support::recorded(
        target,
        "library",
        &sources,
        args,
        |scratch| {
            let (run, produced) = run_live(target, scratch, paths, args, false)?;
            let mut files = BTreeMap::new();
            collect_files(&produced, &produced, &mut files);
            files.insert(STATUS.to_owned(), run.status.to_string().into_bytes());
            files.insert(STDERR.to_owned(), run.stderr.into_bytes());
            Some(files)
        },
    )?;
    let status = std::str::from_utf8(files.get(STATUS)?)
        .ok()?
        .parse()
        .expect("a recorded reference status is an integer");
    let stderr = String::from_utf8_lossy(files.get(STDERR)?).into_owned();
    for (name, bytes) in &files {
        if name == STATUS || name == STDERR {
            continue;
        }
        let path = out.join(name);
        std::fs::create_dir_all(path.parent().expect("a recorded entry has a parent"))
            .expect("create a replayed library directory");
        std::fs::write(path, bytes).expect("write a replayed library entry");
    }
    Some((Reference { status, stderr }, out))
}

const STATUS: &str = "@status";
const STDERR: &str = "@stderr";

fn collect_files(root: &Path, dir: &Path, files: &mut BTreeMap<String, Vec<u8>>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries {
        let path = entry.expect("read a reference output entry").path();
        if path.is_dir() {
            collect_files(root, &path, files);
        } else {
            let name = path
                .strip_prefix(root)
                .expect("an output entry is under its root")
                .components()
                .map(|part| part.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            files.insert(
                name,
                std::fs::read(&path).expect("read a reference output file"),
            );
        }
    }
}
