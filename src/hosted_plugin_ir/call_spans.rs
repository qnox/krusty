//! Where kotlinc's IR places each call of a body's syntax: a call through a receiver (`a.f(x)`)
//! starts at its callee's name, since the receiver is an element of its own; any other call keeps
//! the span of its syntax.

use crate::ast::{Expr, File};
use crate::diag::Span;
use crate::fx_hash::HashMap;

pub(super) struct CallSpans {
    /// The IR span of each call by the span of its syntax, `None` where two calls share that span.
    spans: HashMap<Span, Option<Span>>,
}

impl CallSpans {
    pub(super) fn new(file: &File) -> Self {
        let mut spans = HashMap::default();
        for (id, expr) in file.expr_arena.iter().enumerate() {
            let Expr::Call { callee, .. } = expr else {
                continue;
            };
            let Some(span) = file.expr_span(crate::ast::ExprId(id as u32)) else {
                continue;
            };
            let ir = match file.expr(*callee) {
                Expr::Member { name, .. } => {
                    Span::new(file.member_name_span(*callee, name).lo, span.hi)
                }
                _ => span,
            };
            spans
                .entry(span)
                .and_modify(|known: &mut Option<Span>| {
                    if *known != Some(ir) {
                        *known = None;
                    }
                })
                .or_insert(Some(ir));
        }
        Self { spans }
    }

    /// The IR span of the call written at `span`, or `None` when no single call is written there.
    pub(super) fn get(&self, span: Span) -> Option<Span> {
        self.spans.get(&span).copied().flatten()
    }
}
