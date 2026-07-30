//! Assembles the three-section debugging dump for one source file: AST, checker result, IR.
//!
//! Every input is a core type, so the whole document is renderable without an editor session. The
//! LSP dump path is one caller; tests and future CLI tooling can be others.

use crate::ast::File;
use crate::diag::Diagnostic;
use crate::frontend::{FrontendSymbols, FrontendTypeInfo};
use crate::ir::IrFile;
use crate::runtime::TargetRuntime;
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

/// One checked file, plus everything lowering it needs.
///
/// This is the input a caller holding a finished analysis already has: the callers must not have to
/// know how the IR section is produced, only which file they want dumped.
pub struct FileDumpInput<'a> {
    /// Workspace-relative path shown in the heading.
    pub label: &'a str,
    pub source: &'a str,
    pub file: &'a File,
    /// Index of `file` within the analyzed source set; lowering stamps it into IR spans.
    pub file_index: usize,
    pub info: Option<&'a FrontendTypeInfo>,
    pub symbols: &'a FrontendSymbols,
    pub runtime: &'a dyn TargetRuntime,
    pub diagnostics: &'a [Diagnostic],
}

/// Lower the checked file and render its dump document.
///
/// Lowering lives here rather than in the caller because the IR section owns its own failure modes:
/// an unchecked file, a reported bail, and a silent `None` all have to become one displayable
/// reason, and that mapping is a property of the document, not of whoever asked for it.
pub fn render_file_dump(input: &FileDumpInput<'_>) -> String {
    let bail = std::cell::RefCell::new(String::new());
    let lowered = input.info.and_then(|info| {
        crate::ir_lower::lower_file_at_reporting(
            input.file,
            input.file_index as u32,
            info,
            input.symbols,
            input.runtime,
            &bail,
        )
    });
    let bail_reason = bail.borrow();
    let ir = match lowered.as_ref() {
        Some(ir) => Ok(ir),
        None if input.info.is_none() => Err("file was not checked"),
        None if bail_reason.is_empty() => Err("lowering produced no IR and no reason"),
        None => Err(bail_reason.as_str()),
    };

    render_dump(&DumpInput {
        label: input.label,
        source: input.source,
        file: input.file,
        info: input.info,
        diagnostics: input.diagnostics,
        ir,
    })
}

/// Render the dump document.
pub fn render_dump(input: &DumpInput<'_>) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "# krusty dump — {}", input.label);
    let _ = writeln!(out, "\nsource bytes: {}", input.source.len());
    let _ = writeln!(out, "source hash: {:016x}", source_hash(input.source));

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

/// Stable, non-cryptographic digest of the text the document was rendered from.
///
/// A dump replays whatever source its producer last analyzed, which can predate the buffer on
/// screen. `source bytes` alone does not reveal that: a length-preserving edit — renaming `foo` to
/// `bar` — leaves the byte count identical, so a stale document reads exactly like a current one.
/// The hash is what a reader compares against the file they are actually looking at.
///
/// Deliberately reuses the compiler's own hasher: dumps are diffed against each other, so the digest
/// has to be identical across runs and processes, and no seeded or randomized hasher would be.
fn source_hash(source: &str) -> u64 {
    use std::hash::Hasher as _;
    let mut hasher = crate::name_tree::FxHasher::default();
    hasher.write(source.as_bytes());
    hasher.finish()
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
    fn the_header_hash_separates_length_preserving_edits() {
        let render = |source: &str| {
            let mut diags = DiagSink::new();
            let files = [parse_source_with_detected_features(source, &mut diags)];
            render_dump(&DumpInput {
                label: "src/Main.kt",
                source,
                file: &files[0],
                info: None,
                diagnostics: &[],
                ir: Err("frontend errors"),
            })
        };

        // Renaming `foo` to `bar` keeps every byte count identical, so `source bytes` cannot tell a
        // stale dump from a current one on its own.
        let before = render("fun foo() {}\n");
        let after = render("fun bar() {}\n");
        let header = |text: &str| {
            text.lines()
                .find(|line| line.starts_with("source hash: "))
                .expect("the header states which text was rendered")
                .to_string()
        };

        assert!(before.contains("source bytes: 13"), "{before}");
        assert!(after.contains("source bytes: 13"), "{after}");
        assert_ne!(header(&before), header(&after));
        // Stable across renders: dumps are diffed against each other.
        assert_eq!(header(&before), header(&render("fun foo() {}\n")));
    }

    #[test]
    fn a_file_dump_lowers_the_checked_file_itself() {
        let source = "fun box(): String = \"OK\"\n";
        let mut diags = DiagSink::new();
        let files = [parse_source_with_detected_features(source, &mut diags)];
        let mut symbols = collect_signatures(&files, &mut diags);
        let info = check_file(&files[0], &mut symbols, &mut diags);

        let text = render_file_dump(&FileDumpInput {
            label: "src/Main.kt",
            source,
            file: &files[0],
            file_index: 0,
            info: Some(&info),
            symbols: &symbols,
            runtime: &EmptySymbolSource,
            diagnostics: &[],
        });

        assert!(text.contains("\n## IR\n"), "{text}");
        assert!(text.contains("functions (1)"), "{text}");
    }

    #[test]
    fn a_file_dump_names_the_reason_an_unchecked_file_has_no_ir() {
        let source = "fun box(): String = \"OK\"\n";
        let mut diags = DiagSink::new();
        let files = [parse_source_with_detected_features(source, &mut diags)];
        let symbols = collect_signatures(&files, &mut diags);

        let text = render_file_dump(&FileDumpInput {
            label: "src/Main.kt",
            source,
            file: &files[0],
            file_index: 0,
            info: None,
            symbols: &symbols,
            runtime: &EmptySymbolSource,
            diagnostics: &[],
        });

        assert!(text.contains("not lowered: file was not checked"), "{text}");
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
