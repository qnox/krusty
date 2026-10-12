//! Reference-compiler invocation and diagnostic rendering shared by test binaries.
//!
//! Product e2e tests and the general reference-target oracle need this boundary. It owns only
//! named fixture materialization, the recorded kotlinc invocation, and exact diagnostic parsing;
//! target conformance compiles its narrower KLIB oracle instead of this e2e support.

use std::path::{Path, PathBuf};

use super::common_core::{byte_dump, kotlinc_compile, scratch_dir};

#[derive(Debug, PartialEq, Eq)]
pub struct CompilerError {
    pub file: String,
    pub line: usize,
    pub column: usize,
    pub message: String,
}

/// Extract every rendered compiler error in emission order. The scratch-directory prefixes differ
/// between invocations, so the stable source filename is the location boundary compared by tests.
pub fn compiler_errors(output: &str) -> Vec<CompilerError> {
    rendered_diagnostics(output, "error:")
}

/// Every rendered compiler warning in emission order, located like [`compiler_errors`].
pub fn compiler_warnings(output: &str) -> Vec<CompilerError> {
    rendered_diagnostics(output, "warning:")
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

fn rendered_diagnostics(output: &str, severity: &str) -> Vec<CompilerError> {
    output
        .lines()
        .filter_map(|rendered| {
            let (location, message) = rendered.split_once(severity)?;
            let location = location.trim().trim_end_matches(':');
            let mut fields = location.rsplitn(3, ':');
            let column = fields.next()?.trim().parse().ok()?;
            let line = fields.next()?.trim().parse().ok()?;
            let path = fields.next()?.trim();
            Some(CompilerError {
                file: Path::new(path)
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or(path)
                    .to_string(),
                line,
                column,
                message: message.trim().to_string(),
            })
        })
        .collect()
}

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

/// Complete error messages already rendered by one compiler invocation. Each diagnostic starts
/// with `file:line:column: first line`; subsequent message lines are prefixed with `| `. Kotlinc's
/// source excerpt and caret are omitted when `excerpted` is true.
pub fn rendered_error_blocks(output: &str, excerpted: bool) -> Vec<String> {
    error_blocks(output, excerpted)
}

/// The error blocks of a reference compiler's stderr, excluding its trailing source and caret.
pub fn reference_error_blocks_from(stderr: &str) -> Vec<String> {
    error_blocks(stderr, true)
}

pub(crate) fn error_blocks(stderr: &str, excerpted: bool) -> Vec<String> {
    let located = |line: &str| {
        line.split_once(": error: ")
            .or_else(|| line.split_once(": warning: "))
            .or_else(|| line.split_once(": info: "))
            .is_some_and(|(location, _)| location.rsplitn(3, ':').count() == 3)
    };
    let mut blocks = Vec::new();
    let mut lines = stderr.lines().peekable();
    while let Some(line) = lines.next() {
        if !located(line) || !line.contains(": error: ") {
            continue;
        }
        let Some(header) = render_errors(line).pop() else {
            continue;
        };
        let mut continuation = Vec::new();
        while let Some(next) = lines.peek() {
            if located(next) || next.starts_with("krusty:") {
                break;
            }
            continuation.push(format!("| {}", lines.next().expect("peeked line")));
        }
        let caret = |line: &String| line.trim_start_matches("| ").trim().starts_with('^');
        if excerpted && continuation.last().is_some_and(caret) {
            continuation.truncate(continuation.len().saturating_sub(2));
        }
        blocks.push(header);
        blocks.extend(continuation);
    }
    blocks
}

pub(crate) fn render_errors(stderr: &str) -> Vec<String> {
    compiler_errors(stderr)
        .into_iter()
        .map(|error| {
            format!(
                "{}:{}:{}: {}",
                error.file, error.line, error.column, error.message
            )
        })
        .collect()
}
