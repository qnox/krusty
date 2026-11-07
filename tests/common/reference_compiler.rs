//! Reference-compiler invocation and diagnostic rendering shared by test binaries.
//!
//! Product e2e tests and the general reference-target oracle need this boundary. It owns only
//! named fixture materialization, the recorded kotlinc invocation, and exact diagnostic parsing;
//! target conformance compiles its narrower KLIB oracle instead of this e2e support.

use std::path::{Path, PathBuf};

use super::common_core::{byte_dump, kotlinc_compile, scratch_dir};

pub use super::common_core::reference_diagnostics::{compiler_errors, CompilerError};
pub(crate) use super::common_core::reference_diagnostics::{error_blocks, render_errors};

/// Write named sources under `work`, creating their directories, and return their paths.
pub fn write_fixture_sources(work: &Path, sources: &[(&str, &str)]) -> Vec<PathBuf> {
    sources
        .iter()
        .map(|(name, source)| {
            let path = work.join(name);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("create fixture source directory");
            }
            std::fs::write(&path, source).expect("write compiler fixture");
            path
        })
        .collect()
}

pub(crate) fn kotlinc_paths_result(
    sources: &[PathBuf],
    output: &Path,
    extra_args: &[String],
) -> (i32, String) {
    let mut args = sources
        .iter()
        .map(|path| path.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    args.extend(["-d".to_string(), output.to_string_lossy().into_owned()]);
    args.extend_from_slice(extra_args);
    byte_dump::with_recorded_diagnostics(|| {
        kotlinc_compile(&args).expect("reference compiler unavailable")
    })
}

/// kotlinc's exit status and stderr for named sources, recorded like every reference diagnostic
/// run.
pub fn reference_compiler_run(sources: &[(&str, &str)], extra_args: &[String]) -> (i32, String) {
    let work = scratch_dir().expect("cannot allocate reference-compiler fixture");
    let source_paths = write_fixture_sources(&work, sources);
    let result = kotlinc_paths_result(&source_paths, &work.join("out"), extra_args);
    let _ = std::fs::remove_dir_all(work);
    result
}

/// Every rendered compiler warning in emission order, located like [`compiler_errors`].
pub fn compiler_warnings(output: &str) -> Vec<CompilerError> {
    super::common_core::reference_diagnostics::rendered_diagnostics(output, "warning:")
}

/// Every warning about the compilation itself rather than a source position (`warning: …` with no
/// location), in emission order.
pub fn compiler_module_warnings(output: &str) -> Vec<String> {
    output
        .lines()
        .filter_map(|rendered| rendered.strip_prefix("warning: "))
        .map(str::to_string)
        .collect()
}

/// Complete error messages already rendered by one compiler invocation. Each diagnostic starts
/// with `file:line:column: first line`; subsequent message lines are prefixed with `| `. Kotlinc's
/// source excerpt and caret are omitted when `excerpted` is true.
pub fn rendered_error_blocks(output: &str, excerpted: bool) -> Vec<String> {
    error_blocks(output, excerpted)
}
