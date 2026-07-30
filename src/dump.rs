//! Assembles the three-section debugging dump for one source file: AST, checker result, IR.
//!
//! Every input is a core type, so the whole document is renderable without an editor session. The
//! LSP dump path is one caller; tests and future CLI tooling can be others.

use crate::ast::File;
use crate::diag::Diagnostic;
use crate::frontend::FrontendTypeInfo;
use crate::ir::IrFile;
use std::fmt::Write as _;

/// Everything one dump needs. `ir` carries the lowered file, or the bail reason explaining why
/// lowering did not produce one.
pub struct DumpInput<'a> {
    /// Workspace-relative path shown in the heading.
    pub label: &'a str,
    pub source: &'a str,
    pub file: &'a File,
    pub info: Option<&'a FrontendTypeInfo>,
    pub diagnostics: &'a [Diagnostic],
    pub ir: Result<&'a IrFile, &'a str>,
}

/// Render the dump document.
pub fn render_dump(input: &DumpInput<'_>) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "# krusty dump — {}", input.label);
    let _ = writeln!(out, "\nsource bytes: {}", input.source.len());

    let _ = writeln!(out, "\n## AST\n");
    let _ = writeln!(out, "```");
    out.push_str(&crate::ast_print::render_file(input.file));
    let _ = writeln!(out, "```");

    let _ = writeln!(out, "\n## Checker\n");
    let _ = writeln!(out, "```");
    if input.diagnostics.is_empty() {
        let _ = writeln!(out, "no diagnostics");
    } else {
        for diagnostic in input.diagnostics {
            let span = diagnostic.editor_span.unwrap_or(diagnostic.span);
            let (line, column) = line_column(input.source, span.lo as usize);
            let _ = writeln!(
                out,
                "{line}:{column} {} {}",
                severity_label(diagnostic.severity),
                diagnostic.msg
            );
        }
    }
    match input.info {
        Some(info) => {
            let _ = writeln!(out, "\ntyped expressions ({})", info.expr_types.len());
            for (id, ty) in info.expr_types.iter().enumerate() {
                let span = match input.file.expr_spans.get(id) {
                    Some(span) => format!("{}..{}", span.lo, span.hi),
                    None => "?..?".to_string(),
                };
                let _ = writeln!(out, "  [{id}] {span} {ty:?}");
            }
        }
        None => {
            let _ = writeln!(out, "\ntyped expressions: <file not checked>");
        }
    }
    let _ = writeln!(out, "```");

    let _ = writeln!(out, "\n## IR\n");
    let _ = writeln!(out, "```");
    match input.ir {
        Ok(ir) => out.push_str(&crate::ir_print::render_ir_file(ir)),
        Err(reason) => {
            let _ = writeln!(out, "not lowered: {reason}");
        }
    }
    let _ = writeln!(out, "```");

    out
}

fn severity_label(severity: crate::diag::Severity) -> &'static str {
    match severity {
        crate::diag::Severity::Error => "error",
        crate::diag::Severity::Warning => "warning",
    }
}

/// 1-based line and column for a byte offset, clamped to the end of `source`.
fn line_column(source: &str, offset: usize) -> (u32, u32) {
    let offset = offset.min(source.len());
    let mut line = 1u32;
    let mut column = 1u32;
    for byte in source.as_bytes()[..offset].iter().copied() {
        if byte == b'\n' {
            line += 1;
            column = 1;
        } else {
            column += 1;
        }
    }
    (line, column)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diag::{DiagSink, Severity, Span};
    use crate::frontend::{check_file, collect_signatures, parse_source_with_detected_features};
    use crate::ir_lower::lower_file;
    use crate::libraries::EmptySymbolSource;

    #[test]
    fn renders_all_three_sections_with_a_lowered_ir() {
        let source = "fun box(): String = \"OK\"\n";
        let mut diags = DiagSink::new();
        let files = [parse_source_with_detected_features(source, &mut diags)];
        let mut symbols = collect_signatures(&files, &mut diags);
        let info = check_file(&files[0], &mut symbols, &mut diags);
        let ir = lower_file(&files[0], &info, &symbols, &EmptySymbolSource).expect("lowers");

        let text = render_dump(&DumpInput {
            label: "src/Main.kt",
            source,
            file: &files[0],
            info: Some(&info),
            diagnostics: &[],
            ir: Ok(&ir),
        });

        assert!(text.starts_with("# krusty dump — src/Main.kt\n"), "{text}");
        assert!(text.contains("\n## AST\n"), "{text}");
        assert!(text.contains("\n## Checker\n"), "{text}");
        assert!(text.contains("\n## IR\n"), "{text}");
        assert!(text.contains("no diagnostics"), "{text}");
        assert!(text.contains("functions (1)"), "{text}");
        // No wall-clock reading: dumps get diffed against each other.
        assert!(!text.contains("generated at"), "{text}");
    }

    #[test]
    fn ir_bail_reason_replaces_the_ir_body() {
        let source = "fun box(): String = \"OK\"\n";
        let mut diags = DiagSink::new();
        let files = [parse_source_with_detected_features(source, &mut diags)];
        let mut symbols = collect_signatures(&files, &mut diags);
        let info = check_file(&files[0], &mut symbols, &mut diags);

        let text = render_dump(&DumpInput {
            label: "src/Main.kt",
            source,
            file: &files[0],
            info: Some(&info),
            diagnostics: &[],
            ir: Err("lower_expr: unsupported Expr::Wild"),
        });

        assert!(
            text.contains("not lowered: lower_expr: unsupported Expr::Wild"),
            "{text}"
        );
        assert!(!text.contains("functions ("), "{text}");
    }

    #[test]
    fn diagnostics_render_with_one_based_line_and_column() {
        let source = "fun box(): String {\n    return 1\n}\n";
        let mut diags = DiagSink::new();
        let files = [parse_source_with_detected_features(source, &mut diags)];

        // Byte 31 is the `1` on line 2 (line 2 starts at byte 20: 4 spaces of indent then
        // `return `, so `1` sits at column 12).
        let diagnostic = Diagnostic {
            span: Span::new(31, 32),
            editor_span: None,
            severity: Severity::Error,
            kind: crate::diag::DiagnosticKind::Compiler,
            msg: "type mismatch".to_string(),
            identity: None,
            file: 0,
        };

        let text = render_dump(&DumpInput {
            label: "src/Main.kt",
            source,
            file: &files[0],
            info: None,
            diagnostics: &[diagnostic],
            ir: Err("frontend errors"),
        });

        assert!(text.contains("2:12 error type mismatch"), "{text}");
    }

    #[test]
    fn line_and_column_are_one_based_from_the_file_start() {
        assert_eq!(line_column("abc\ndef\n", 0), (1, 1));
        assert_eq!(line_column("abc\ndef\n", 3), (1, 4));
        assert_eq!(line_column("abc\ndef\n", 4), (2, 1));
        // Past the end clamps to the last position rather than panicking.
        assert_eq!(line_column("abc\n", 999), (2, 1));
    }
}
