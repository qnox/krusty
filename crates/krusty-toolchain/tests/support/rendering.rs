//! The problems in what `kotlin` printed, read back from its rendering into the form
//! krusty-toolchain's diagnostics are compared in: `<file>:<line>:<column>: <SEVERITY>: <message>`,
//! the file relative to the project root and line breaks in the message written as `\n`.
//!
//! The toolchain draws a problem in a file as a box: `╭─ SEVERITY: message`, more message lines
//! `│ text`, the location `│ → file:line:column`, the quoted source, and `╰─`. A problem with a
//! whole file is a `SEVERITY: message` line followed by ` ╰→ file`; one with the project as a whole
//! is a `SEVERITY: message` line whose message runs to the next blank line. The closing line saying
//! the command stopped is not a problem, and neither is the table a successful command prints.

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

/// The table a successful command printed, byte for byte: from a line starting `╭` to the line
/// starting `╰` that closes it, its line break included. A problem's box is indented, so it is
/// never taken for the table.
pub fn table(output: &[u8]) -> Option<&[u8]> {
    let mut start = None;
    let mut offset = 0;
    for line in output.split_inclusive(|&byte| byte == b'\n') {
        if start.is_none() && line.starts_with("╭".as_bytes()) {
            start = Some(offset);
        }
        offset += line.len();
        if start.is_some() && line.starts_with("╰".as_bytes()) {
            return Some(&output[start?..offset]);
        }
    }
    None
}

/// The problems `output` reports, in order.
pub fn problems(root: &str, output: &[u8]) -> Vec<String> {
    let text = String::from_utf8_lossy(output);
    let lines: Vec<&str> = text.lines().collect();
    let mut problems = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        let line = lines[index];
        if let Some((severity, first)) = line.trim_start().strip_prefix("╭─ ").and_then(severity)
        {
            let mut message = vec![first.to_string()];
            let mut place = String::new();
            while let Some(text) = lines.get(index + 1).and_then(|line| box_text(line)) {
                index += 1;
                if let Some(found) = location(root, text) {
                    place = format!("{found}: ");
                    break;
                }
                if !text.is_empty() || lines[index].trim_start() != "│" {
                    message.push(text.to_string());
                }
            }
            while !lines[index].trim_start().starts_with("╰─") {
                index += 1;
            }
            problems.push(format!("{place}{severity}: {}", message.join("\\n")));
        } else if let Some((severity, first)) = severity(line).filter(|_| !ABORT.contains(&line)) {
            let file = lines
                .get(index + 1)
                .and_then(|next| next.strip_prefix(" ╰→ "));
            if let Some(file) = file {
                index += 1;
                let file = file.replace(&format!("{root}/"), "");
                problems.push(format!("{file}: {severity}: {first}"));
            } else {
                let mut message = vec![first.to_string()];
                while let Some(next) = lines.get(index + 1).filter(|next| !next.trim().is_empty()) {
                    index += 1;
                    message.push(next.replace(&format!("{root}/"), ""));
                }
                problems.push(format!("{severity}: {}", message.join("\\n")));
            }
        }
        index += 1;
    }
    problems
}
