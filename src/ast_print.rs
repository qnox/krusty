//! Flat, id-ordered text rendering of a parsed `File`'s arenas.
//!
//! The AST is index-based: nodes reference each other by `u32` arena ids. This printer therefore
//! lists each arena in id order rather than reconstructing a tree — every node appears exactly once,
//! including nodes unreachable from a declaration, and each node's `Debug` form already names its
//! variant and prints its child ids.

use crate::ast::File;
use crate::diag::Span;
use std::fmt::Write as _;

/// Render every arena of `file` as text.
pub fn render_file(file: &File) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "package: {}",
        file.package.as_deref().unwrap_or("<none>")
    );
    let _ = writeln!(out, "is_script: {}", file.is_script);
    let _ = writeln!(out, "source_line_count: {}", file.source_line_count);
    if !file.imports.is_empty() {
        let _ = writeln!(out, "imports: {:?}", file.imports);
    }
    if !file.import_aliases.is_empty() {
        let _ = writeln!(out, "import_aliases: {:?}", file.import_aliases);
    }
    let _ = writeln!(out, "decls: {:?}", file.decls);
    if !file.expect_decls.is_empty() {
        let _ = writeln!(out, "expect_decls: {:?}", file.expect_decls);
    }

    let _ = writeln!(out, "\ndecl_arena ({})", file.decl_arena.len());
    for (id, decl) in file.decl_arena.iter().enumerate() {
        let _ = writeln!(out, "  [{id}] {decl:?}");
    }

    let _ = writeln!(out, "\nexpr_arena ({})", file.expr_arena.len());
    for (id, expr) in file.expr_arena.iter().enumerate() {
        let span = slot(&file.expr_spans, id);
        let line = slot_line(&file.expr_lines, id);
        let _ = writeln!(out, "  [{id}] {span} {line} {expr:?}");
    }

    let _ = writeln!(out, "\nstmt_arena ({})", file.stmt_arena.len());
    for (id, stmt) in file.stmt_arena.iter().enumerate() {
        let span = slot(&file.stmt_spans, id);
        let line = slot_line(&file.stmt_lines, id);
        let _ = writeln!(out, "  [{id}] {span} {line} {stmt:?}");
    }

    out
}

/// `lo..hi` for a parallel span vector, or `?..?` when the slot is absent.
fn slot(spans: &[Span], id: usize) -> String {
    match spans.get(id) {
        Some(span) => format!("{}..{}", span.lo, span.hi),
        None => "?..?".to_string(),
    }
}

/// `line=N` for a parallel line vector; `line=?` when absent or unknown (0).
fn slot_line(lines: &[u32], id: usize) -> String {
    match lines.get(id) {
        Some(0) | None => "line=?".to_string(),
        Some(line) => format!("line={line}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diag::DiagSink;
    use crate::frontend::parse_source_with_detected_features;

    fn parse(source: &str) -> File {
        let mut diags = DiagSink::new();
        let file = parse_source_with_detected_features(source, &mut diags);
        assert!(!diags.has_errors(), "{:?}", diags.diags);
        file
    }

    #[test]
    fn renders_header_and_every_arena_slot() {
        let file = parse("package demo\n\nfun box(): String = \"OK\"\n");
        let text = render_file(&file);

        assert!(text.contains("package: demo"), "{text}");
        assert!(text.contains("decls: [DeclId(0)]"), "{text}");
        // One line per slot, in id order, with the arena counts stated up front.
        assert!(
            text.contains(&format!("decl_arena ({})", file.decl_arena.len())),
            "{text}"
        );
        assert!(
            text.contains(&format!("expr_arena ({})", file.expr_arena.len())),
            "{text}"
        );
        for id in 0..file.expr_arena.len() {
            assert!(
                text.contains(&format!("\n  [{id}] ")),
                "missing expr {id}:\n{text}"
            );
        }
    }

    #[test]
    fn expression_lines_carry_span_and_debug_form() {
        let file = parse("fun box(): String = \"OK\"\n");
        let text = render_file(&file);

        // Every expression line is `  [id] lo..hi line=N Debug`.
        assert!(text.contains("StringLit("), "{text}");
        assert!(text.contains("line="), "{text}");
    }

    #[test]
    fn absent_package_is_rendered_explicitly() {
        let file = parse("fun box(): String = \"OK\"\n");
        assert!(render_file(&file).contains("package: <none>"));
    }
}
