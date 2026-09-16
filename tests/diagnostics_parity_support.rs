//! Shared exact diagnostic observations for the responsibility-focused parity suites.

use std::path::Path;

use super::common;

#[derive(Debug, PartialEq, Eq)]
pub(super) struct ObservedError {
    pub(super) file: String,
    pub(super) line: usize,
    pub(super) column: usize,
    pub(super) message: String,
}

pub(super) const RECURSIVE_INFERENCE_MESSAGE: &str = "type checking has run into a recursive problem. Easiest workaround: specify the types of your declarations explicitly.";

/// Extract every `path:line:column: error: message` record. Compare the basename because both
/// compilers receive the same path, while their renderers may independently canonicalize its prefix.
pub(super) fn errors(output: &str) -> Vec<ObservedError> {
    output
        .lines()
        .filter_map(|rendered| {
            let (location, message) = rendered.split_once("error:")?;
            let location = location.trim().trim_end_matches(':');
            let mut fields = location.rsplitn(3, ':');
            let column = fields.next()?.trim().parse().ok()?;
            let line = fields.next()?.trim().parse().ok()?;
            let path = fields.next()?.trim();
            let file = Path::new(path)
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or(path)
                .to_string();
            Some(ObservedError {
                file,
                line,
                column,
                message: message.trim().to_string(),
            })
        })
        .collect()
}

pub(super) fn first_error(output: &str) -> Option<ObservedError> {
    errors(output).into_iter().next()
}

/// Compare a bounded semantic family serially. The e2e planner assigns each caller's top-level
/// module independently, so expensive families run in separate processes without multiplying the
/// reference compiler's classpath indexes inside one process.
pub(super) fn assert_error_parity(cases: &[&str]) {
    let stdlib = common::stdlib_jar();
    let mut mismatches = Vec::new();
    for (index, source) in cases.iter().enumerate() {
        let file = format!("t{index}.kt");
        let result =
            common::compiler_diagnostics(&[(file.as_str(), source)], std::slice::from_ref(&stdlib));
        let krusty =
            first_error(&result.krusty_stderr).or_else(|| first_error(&result.krusty_stdout));
        let reference = first_error(&result.reference_stderr);
        if (result.krusty_code == 0) != (result.reference_code == 0) || krusty != reference {
            mismatches.push(format!(
                "diagnostic mismatch for {source:?}\n krusty ({code}): {krusty:?}\n kotlinc ({reference_code}): {reference:?}",
                code = result.krusty_code,
                reference_code = result.reference_code,
            ));
        }
    }
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n\n"));
}
