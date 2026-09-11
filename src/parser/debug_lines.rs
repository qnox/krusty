//! Source-line metadata attached while parsed syntax and exact spans are simultaneously live.

use crate::ast::{Decl, Expr, File, FunBody};
use crate::diag::Span;

/// Attach 1-based source lines to syntax nodes and declarations after parsing. The emitter consumes
/// this metadata only after declarations have been rebound to stable FIR/IR identities. Uses one
/// newline index for every span lookup, so the whole arena costs one pass over `src` plus lookups.
pub(super) fn attach(file: &mut File, src: &str) {
    // `line_starts[k]` = byte offset where line (k+1) begins; line 1 starts at 0.
    let mut line_starts = vec![0u32];
    for (i, b) in src.bytes().enumerate() {
        if b == b'\n' {
            line_starts.push(i as u32 + 1);
        }
    }
    file.source_line_count = line_starts.len() as u32;
    let line_at = |off: u32| -> u32 {
        // 1-based line: index of the last start <= off.
        match line_starts.binary_search(&off) {
            Ok(k) => k as u32 + 1,
            Err(k) => k as u32, // k = count of starts strictly <= off (since off not found)
        }
    };
    let span_line_at = |span: Span, off: u32| {
        if span.lo == 0 && span.hi == 0 {
            0
        } else {
            line_at(off)
        }
    };
    // Per-node start lines for the statement-level `LineNumberTable` mapping.
    file.expr_lines = file
        .expr_spans
        .iter()
        .map(|&span| span_line_at(span, span.lo))
        .collect();
    file.expr_source_lines = file
        .expr_arena
        .iter()
        .enumerate()
        .map(|(index, expr)| {
            let member_name_span = |index: usize, name: &str| {
                file.exact_member_name_spans
                    .get(&(index as u32))
                    .copied()
                    .unwrap_or_else(|| {
                        let span = file.expr_spans[index];
                        Span::new(span.hi.saturating_sub(name.len() as u32), span.hi)
                    })
            };
            let anchor = match expr {
                Expr::Call { callee, .. } => match &file.expr_arena[callee.0 as usize] {
                    Expr::Member { name, .. } | Expr::SafeCall { name, .. } => {
                        member_name_span(callee.0 as usize, name)
                    }
                    _ => file.expr_spans[callee.0 as usize],
                },
                Expr::Member { name, .. } | Expr::SafeCall { name, .. } => {
                    member_name_span(index, name)
                }
                _ => file.expr_spans[index],
            };
            span_line_at(anchor, anchor.lo)
        })
        .collect();
    file.expr_end_lines = file
        .expr_spans
        .iter()
        .map(|&span| span_line_at(span, span.hi))
        .collect();
    file.stmt_lines = file
        .stmt_spans
        .iter()
        .map(|&span| span_line_at(span, span.lo))
        .collect();
    // Snapshot each expression's start offset before the mutable walk of `decl_arena` (a disjoint
    // field, but only borrowck's field-splitting sees that — a helper can't).
    let expr_lo: Vec<u32> = file.expr_spans.iter().map(|s| s.lo).collect();
    let expr_hi: Vec<u32> = file.expr_spans.iter().map(|s| s.hi).collect();
    // kotlinc attributes a method's `LineNumberTable` to its BODY, not the `fun` keyword — for an
    // EXPRESSION body that is the `=` expression's line, which differs from the declaration line when
    // the signature wraps across lines. A block body is left at the declaration line (its per-statement
    // mapping is a separate, larger matter); a single-line expression body is unchanged (same line).
    let body_line = |body: &FunBody, decl: u32| -> u32 {
        match body {
            FunBody::Expr(e) => expr_lo.get(e.0 as usize).map_or(decl, |&lo| line_at(lo)),
            _ => decl,
        }
    };
    // A BLOCK body's closing `}` line — kotlinc maps a `Unit` fn's implicit `return` there.
    let body_close = |body: &FunBody| -> u32 {
        match body {
            FunBody::Block(e) => {
                // A synthesized block with the zero span stays "unknown" (mirror the
                // `expr_lines`/`stmt_lines` guard) — `line_at(0)` would fabricate line 1.
                let (lo, hi) = expr_lo
                    .get(e.0 as usize)
                    .copied()
                    .zip(expr_hi.get(e.0 as usize).copied())
                    .unwrap_or((0, 0));
                if lo == 0 && hi == 0 {
                    0
                } else {
                    line_at(hi.saturating_sub(1))
                }
            }
            _ => 0,
        }
    };
    for decl in &mut file.decl_arena {
        match decl {
            Decl::Class(c) => {
                c.decl_line = line_at(c.span.lo);
                // Where the DECLARATION starts, annotations included: kotlinc maps the primary
                // constructor's `super()` call there, while the trailing `return` goes back to the
                // class HEADER line above. The two differ exactly when an annotation sits on its own
                // line (`@Serializable` over `data class Foo`). `line_at` is a binary search, so an
                // out-of-order offset here is fine.
                let start = c
                    .annotations
                    .iter()
                    .map(|annotation| annotation.span.lo)
                    .filter(|lo| *lo != 0)
                    .chain(std::iter::once(c.span.lo))
                    .min()
                    .unwrap_or(c.span.lo);
                c.decl_start_line = line_at(start);
                // The parser stored the primary ctor's `)` OFFSET here — rewrite it to the line.
                if c.ctor_close_line != 0 {
                    c.ctor_close_line = line_at(c.ctor_close_line);
                }
                // The class BODY's closing `}` — where kotlinc maps a generated `<clinit>`'s
                // trailing `return`. The span's `hi` is the offset PAST the brace.
                c.body_close_line = line_at(c.span.hi.saturating_sub(1));
                // A class's methods live INSIDE the class decl, not in `decl_arena` — walk them too,
                // or every member method keeps line 0 and gets no `LineNumberTable`.
                for m in &mut c.methods {
                    m.sig_line = line_at(m.span.lo);
                    m.decl_line = body_line(&m.body, m.sig_line);
                    m.body_close_line = body_close(&m.body);
                }
                for p in &mut c.props {
                    if p.span.lo != 0 || p.span.hi != 0 {
                        p.decl_line = line_at(p.span.lo);
                    }
                }
                for p in &mut c.body_props {
                    p.decl_line = line_at(p.span.lo);
                }
                for e in &mut c.enum_entries {
                    e.decl_line = line_at(e.span.lo);
                }
            }
            Decl::Fun(f) => {
                f.sig_line = line_at(f.span.lo);
                f.decl_line = body_line(&f.body, f.sig_line);
                f.body_close_line = body_close(&f.body);
            }
            // A top-level property's accessors and `<clinit>` store map to its declaration line.
            Decl::Property(p) => {
                p.decl_line = line_at(p.span.lo);
            }
        }
    }
}
