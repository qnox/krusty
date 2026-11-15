//! Exact reference-compiler diagnostic parsing shared by every test binary.
use std::path::Path;

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

pub(crate) fn rendered_diagnostics(output: &str, severity: &str) -> Vec<CompilerError> {
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
