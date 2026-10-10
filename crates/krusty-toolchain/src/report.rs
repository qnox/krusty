//! A problem as the toolchain prints it on a terminal that is not interactive
//! (`RichTerminalProblemReporter`): a problem at a place in a file is a box quoting the source
//! with the place marked, one with a whole file names the file under its message, one with the
//! project as a whole is its message alone after its severity, and a conflict between values is
//! its message alone.

use std::fs;
use std::path::Path;

use crate::diagnostic::{Diagnostic, Severity};
use crate::yaml::Span;

/// The narrowest line-number gutter, so most boxes line up.
const MIN_GUTTER_WIDTH: usize = 3;

fn label(severity: Severity) -> &'static str {
    match severity {
        Severity::Error => "ERROR: ",
        Severity::Warning => "WARNING: ",
        Severity::WeakWarning => "WEAK WARNING: ",
    }
}

/// `diagnostic` as the toolchain prints it, without the final line break. Files are named
/// relative to `root` when it is given and contains them, and as they are otherwise.
pub fn render(diagnostic: &Diagnostic, root: Option<&Path>) -> String {
    let label = label(diagnostic.severity);
    if diagnostic.bare {
        return diagnostic.message.clone();
    }
    let Some(file) = diagnostic.file.as_deref() else {
        let continuation = format!("\n{}", " ".repeat(label.len()));
        return format!("{label}{}", diagnostic.message.replace('\n', &continuation));
    };
    let mut location = root
        .and_then(|root| file.strip_prefix(root).ok())
        .unwrap_or(file)
        .display()
        .to_string();
    if let Some(span) = diagnostic.span {
        location.push_str(&format!(":{}:{}", span.start.line, span.start.column));
    }
    // A place past the file's last line quotes nothing, and is named under the message.
    let quoted = diagnostic
        .span
        .and_then(|span| snippet(file, span).map(|lines| (span, lines)));
    let Some((span, lines)) = quoted else {
        return format!("{label}{}\n ╰→ {location}", diagnostic.message);
    };
    let last_line = span.start.line + lines.len() - 1;
    let gutter = last_line.to_string().len().max(MIN_GUTTER_WIDTH);
    let border = " ".repeat(gutter + 1);
    let mut message = diagnostic.message.lines();
    let mut out = vec![format!(
        "{border}╭─ {label}{}",
        message.next().unwrap_or_default()
    )];
    let mut multiline_message = false;
    for line in message {
        multiline_message = true;
        out.push(format!("{border}│ {line}"));
    }
    if multiline_message {
        out.push(format!("{border}│"));
    }
    out.push(format!("{border}│ → {location}"));
    out.push(format!("{border}│"));
    let multiline = lines.len() > 1;
    if multiline {
        let padding = span.start.column - 1;
        let length = lines[0].chars().count().saturating_sub(padding);
        out.push(format!(
            "{border}│ {}{}",
            " ".repeat(padding),
            "⌄".repeat(length)
        ));
    }
    for (index, line) in lines.iter().enumerate() {
        let number = span.start.line + index;
        out.push(format!("{number:>gutter$} │ {line}"));
    }
    out.push(format!("{border}│ {}", bottom_pointer(span, multiline)));
    out.push(format!("{border}╰─"));
    out.join("\n")
}

/// The lines of `file` that `span` covers; `None` when they cannot be read.
fn snippet(file: &Path, span: Span) -> Option<Vec<String>> {
    let text = fs::read_to_string(file).ok()?;
    let count = span.end.line.max(span.start.line) - span.start.line + 1;
    let lines: Vec<String> = text
        .lines()
        .skip(span.start.line - 1)
        .take(count)
        .map(str::to_string)
        .collect();
    (!lines.is_empty()).then_some(lines)
}

/// The marks under the quoted source: under the span on its one line, or up to its end on the
/// last of several.
fn bottom_pointer(span: Span, multiline: bool) -> String {
    if multiline {
        return "⌃".repeat(span.end.column.saturating_sub(1).max(1));
    }
    let length = span.end.column.saturating_sub(span.start.column).max(1);
    format!(
        "{}{}",
        " ".repeat(span.start.column - 1),
        "⌃".repeat(length)
    )
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::yaml::Position;

    fn span(start: (usize, usize), end: (usize, usize)) -> Span {
        Span {
            start: Position {
                line: start.0,
                column: start.1,
            },
            end: Position {
                line: end.0,
                column: end.1,
            },
        }
    }

    fn written(name: &str, text: &str) -> PathBuf {
        let directory = std::env::temp_dir().join(format!(
            "krusty-toolchain-report-{name}-{}",
            std::process::id()
        ));
        fs::create_dir_all(&directory).expect("create the directory");
        let file = directory.join("module.yaml");
        fs::write(&file, text).expect("write the file");
        file
    }

    #[test]
    fn a_problem_at_a_place_quotes_the_line_and_marks_the_span() {
        let file = written("single", "settings:\n  jvm:\n    release: twenty\n");
        let root = file.parent().map(Path::to_path_buf);
        let diagnostic = Diagnostic::error(
            &file,
            Some(span((3, 14), (3, 20))),
            "Expected: integer | null",
        );
        assert_eq!(
            render(&diagnostic, root.as_deref()),
            "    ╭─ ERROR: Expected: integer | null\n    │ → module.yaml:3:14\n    │\n  3 │     release: twenty\n    │              ⌃⌃⌃⌃⌃⌃\n    ╰─"
        );
    }

    #[test]
    fn a_span_over_several_lines_is_marked_from_its_start_and_to_its_end() {
        let file = written("multi", "a:\n  b: [x,\n    y]\n");
        let diagnostic = Diagnostic::warning(
            Severity::Warning,
            &file,
            Some(span((2, 6), (3, 7))),
            "first\nsecond",
        );
        assert_eq!(
            render(&diagnostic, file.parent()),
            "    ╭─ WARNING: first\n    │ second\n    │\n    │ → module.yaml:2:6\n    │\n    │      ⌄⌄⌄\n  2 │   b: [x,\n  3 │     y]\n    │ ⌃⌃⌃⌃⌃⌃\n    ╰─"
        );
    }

    #[test]
    fn a_problem_without_a_place_names_the_file_or_nothing() {
        let file = PathBuf::from("/nowhere/module.yaml");
        assert_eq!(
            render(&Diagnostic::error(&file, None, "Broken"), None),
            "ERROR: Broken\n ╰→ /nowhere/module.yaml"
        );
        let empty = written("empty", "");
        assert_eq!(
            render(
                &Diagnostic::error(&empty, Some(span((2, 1), (2, 1))), "Truncated"),
                empty.parent()
            ),
            "ERROR: Truncated\n ╰→ module.yaml:2:1"
        );
        assert_eq!(
            render(&Diagnostic::project_error("one\ntwo"), None),
            "ERROR: one\n       two"
        );
        assert_eq!(
            render(&Diagnostic::conflict("Conflicting\n  - a"), None),
            "Conflicting\n  - a"
        );
    }
}
