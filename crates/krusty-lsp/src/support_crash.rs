//! Whether the module support compiled with a chunk, and not the chunk's own files, crashes the
//! worker.
//!
//! Disk support is every Kotlin and Java source the module walk contributes after the chunk's
//! own paths are excluded. Those files accompany every half of a split. Compiling them with no
//! chunk file distinguishes that shared crash from two chunk files that each crash on their own.

use std::io;
use std::path::PathBuf;

use krusty::source::SourceInput;
use krusty_lsp::{
    AnalysisWorker, DigestedJavaSource, DigestedSource, LspOptions, ProjectModel,
    WorkerAnalysisOptions,
};

use crate::analysis_finish::remains_pending;
use crate::analysis_group::{script_stem, source_kind_from_uri};
use crate::project_analysis_group::project_group_compiler_config;

pub(super) enum Probe {
    /// The support sources compiled.
    Clean,
    /// The worker crashed while compiling the support sources.
    Crashed,
    /// The support pass timed out or was interrupted. That is not evidence of a shared crash.
    Pending,
}

pub(super) struct DiskSupport<'a> {
    pub disk: &'a [DigestedSource],
    pub inferred_count: usize,
    pub java: &'a [&'a DigestedJavaSource],
}

/// Compile `support` the way a chunk pass compiles it: signatures of the inferred prefix and of
/// the dependency tail, and no body check of the support files themselves.
pub(super) fn compile_disk_support(
    worker: &mut AnalysisWorker,
    model: Option<&ProjectModel>,
    module_index: Option<usize>,
    platform_classpath: &[PathBuf],
    options: &LspOptions,
    support: DiskSupport<'_>,
) -> Probe {
    if support.disk.is_empty() && support.java.is_empty() {
        return Probe::Clean;
    }
    let kotlin = support
        .disk
        .iter()
        .map(|source| (source.uri().to_string(), source.text().to_string()))
        .collect::<Vec<_>>();
    let java = support
        .java
        .iter()
        .map(|source| source.text().to_string())
        .collect::<Vec<_>>();
    let inferred = support.inferred_count.min(kotlin.len());
    let stems = kotlin
        .iter()
        .map(|(uri, _)| script_stem(uri))
        .collect::<Vec<_>>();
    let inputs = kotlin
        .iter()
        .zip(&stems)
        .map(|((uri, text), stem)| {
            let input = SourceInput::new(source_kind_from_uri(uri), text.as_str());
            match stem.as_deref() {
                Some(stem) => input.with_file_stem(stem),
                None => input,
            }
        })
        .collect::<Vec<_>>();
    let (classpath, language_arguments) =
        project_group_compiler_config(model, module_index, platform_classpath, options);
    // A support set that does not fit the worker is not a compiler crash. Isolation continues.
    match worker.analyze_inputs_prefix_with_config(
        &inputs,
        0,
        inferred,
        &java,
        &language_arguments,
        WorkerAnalysisOptions {
            classpath: classpath.as_deref(),
            diagnostics_only: true,
        },
    ) {
        Ok(_) => Probe::Clean,
        Err(error) if error.kind() == io::ErrorKind::InvalidInput => Probe::Clean,
        Err(error) if remains_pending(error.kind()) => Probe::Pending,
        Err(_) => Probe::Crashed,
    }
}
