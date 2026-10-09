//! The problems in what `kotlin` printed, read back from its rendering into the form
//! krusty-toolchain's diagnostics are compared in: `<file>:<line>:<column>: <SEVERITY>: <message>`,
//! the file relative to the project root and line breaks in the message written as `\n`.
//!
//! The toolchain draws a problem in a file as a box: `╭─ SEVERITY: message`, more message lines
//! `│ text`, the location `│ → file:line:column`, the quoted source, and `╰─`. A problem with a
//! whole file is a `SEVERITY: message` line followed by ` ╰→ file`; one with the project as a whole
//! is a `SEVERITY: message` line whose message runs to the next blank line. The closing line saying
//! the command stopped is not a problem. The toolchain writes errors to stderr and warnings, before
//! the command's result, to stdout. A conflict between two values is printed as its message alone,
//! values and places on the indented lines after it; it is an error.

use super::{ExpectedDiagnostic, ExpectedSeverity};

/// A conflict between values is printed without a severity; it is an error.
const CONFLICT: &str = "Conflicting values for property ";
const SEVERITIES: [&str; 3] = ["WEAK WARNING", "WARNING", "ERROR"];
const ABORT: [&str; 2] = [
    "ERROR: Aborting because there were errors in the Kotlin project file, please see above.",
    "ERROR: failed to read Kotlin project model, refer to the errors above",
];

/// `SEVERITY: message` at the start of `text`.
fn severity(text: &str) -> Option<(&'static str, &str)> {
    SEVERITIES.iter().find_map(|&severity| {
        text.strip_prefix(severity)
            .and_then(|rest| rest.strip_prefix(": "))
            .map(|message| (severity, message))
    })
}

fn expected_severity(label: &str) -> ExpectedSeverity {
    match label {
        "ERROR" => ExpectedSeverity::Error,
        "WARNING" => ExpectedSeverity::Warning,
        "WEAK WARNING" => ExpectedSeverity::WeakWarning,
        _ => unreachable!("severity() returns only a known label"),
    }
}

/// A box line's text: `│` and one space dropped, `Some("")` for a bare `│`.
fn box_text(line: &str) -> Option<&str> {
    let rest = line.trim_start().strip_prefix('│')?;
    Some(rest.strip_prefix(' ').unwrap_or(rest))
}

/// `file[:line:column]` from a `→ ` location, the root dropped.
fn location(root: &str, text: &str) -> Option<String> {
    let place = text.strip_prefix("→ ")?;
    Some(place.replace(&format!("{root}/"), ""))
}

/// Whether `line` starts a problem's rendering or is the closing line.
fn starts_problem(line: &str) -> bool {
    ABORT.contains(&line)
        || line.starts_with(CONFLICT)
        || severity(line).is_some()
        || line
            .trim_start()
            .strip_prefix("╭─ ")
            .is_some_and(|rest| severity(rest).is_some())
}

/// The problems rendered at the start of `output`, in order, and the bytes after them: what the
/// command printed as its result. Every line up to that point belongs to a problem's rendering or
/// is the closing line saying the command stopped, so nothing in between is skipped unread.
pub fn problems<'o>(root: &str, output: &'o [u8]) -> (Vec<ExpectedDiagnostic>, &'o [u8]) {
    let mut lines: Vec<(usize, &str)> = Vec::new();
    let mut offset = 0;
    for line in output.split_inclusive(|&byte| byte == b'\n') {
        let text = std::str::from_utf8(line).expect("the toolchain prints UTF-8");
        lines.push((offset, text.strip_suffix('\n').unwrap_or(text)));
        offset += line.len();
    }
    let text = |index: usize| lines.get(index).map(|&(_, text)| text);
    let mut problems = Vec::new();
    let mut index = 0;
    while let Some(line) = text(index) {
        if ABORT.contains(&line) {
            index += 1;
        } else if line.is_empty() && text(index + 1).is_some_and(starts_problem) {
            // A blank line separating problems, or before the closing line.
            index += 1;
        } else if let Some((severity, first)) =
            line.trim_start().strip_prefix("╭─ ").and_then(severity)
        {
            let mut message = vec![first.to_string()];
            let mut place = String::new();
            while let Some(content) = text(index + 1).and_then(box_text) {
                index += 1;
                if let Some(found) = location(root, content) {
                    place = format!("{found}: ");
                    break;
                }
                if !content.is_empty() || lines[index].1.trim_start() != "│" {
                    message.push(content.to_string());
                }
            }
            while !text(index)
                .expect("a problem's box is closed")
                .trim_start()
                .starts_with("╰─")
            {
                index += 1;
            }
            index += 1;
            problems.push(ExpectedDiagnostic {
                severity: expected_severity(severity),
                rendered: format!("{place}{severity}: {}", message.join("\\n")),
            });
        } else if let Some((severity, first)) = severity(line) {
            index += 1;
            if let Some(file) = text(index).and_then(|next| next.strip_prefix(" ╰→ ")) {
                index += 1;
                let file = file.replace(&format!("{root}/"), "");
                problems.push(ExpectedDiagnostic {
                    severity: expected_severity(severity),
                    rendered: format!("{file}: {severity}: {first}"),
                });
            } else {
                // The message runs to the next blank line, which ends it.
                let mut message = vec![first.to_string()];
                while let Some(next) = text(index).filter(|next| !next.trim().is_empty()) {
                    index += 1;
                    message.push(next.replace(&format!("{root}/"), ""));
                }
                if text(index).is_some() {
                    index += 1;
                }
                problems.push(ExpectedDiagnostic {
                    severity: expected_severity(severity),
                    rendered: format!("{severity}: {}", message.join("\\n")),
                });
            }
        } else if line.starts_with(CONFLICT) {
            // Its values and their places run on indented lines, a blank line separating values.
            let mut message = vec![line.replace(&format!("{root}/"), "")];
            index += 1;
            while let Some(next) = text(index).filter(|next| {
                next.starts_with(' ')
                    || next.is_empty()
                        && text(index + 1).is_some_and(|after| after.starts_with("  - "))
            }) {
                index += 1;
                message.push(next.replace(&format!("{root}/"), ""));
            }
            problems.push(ExpectedDiagnostic {
                severity: ExpectedSeverity::Error,
                rendered: format!("ERROR: {}", message.join("\\n")),
            });
        } else {
            break;
        }
    }
    let rest = lines.get(index).map_or(output.len(), |&(offset, _)| offset);
    (problems, &output[rest..])
}
