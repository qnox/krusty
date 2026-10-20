//! Span-keyed lookup of a unit's expressions and statements.
//!
//! Checked bodies name their syntax by source span. A declaration unit holds many bodies, so the
//! index over the whole unit is built once, on first use, rather than once per body.

use std::sync::OnceLock;

use crate::diag::Span;
use crate::fx_hash::HashMap;

use super::File;

/// The raw expression and statement ids at each span. When two nodes share a span, the later one
/// answers.
#[derive(Default)]
pub(crate) struct SpanIndex(OnceLock<SpanIds>);

struct SpanIds {
    expressions: HashMap<Span, u32>,
    statements: HashMap<Span, u32>,
}

fn last_at_each_span(spans: &[Span]) -> HashMap<Span, u32> {
    let mut ids = HashMap::with_capacity_and_hasher(spans.len(), Default::default());
    for (raw, span) in spans.iter().copied().enumerate() {
        ids.insert(span, raw as u32);
    }
    ids
}

impl File {
    fn span_ids(&self) -> &SpanIds {
        self.span_index.0.get_or_init(|| SpanIds {
            expressions: last_at_each_span(&self.expr_spans),
            statements: last_at_each_span(&self.stmt_spans),
        })
    }

    /// The raw id of the last expression at `span`.
    pub(crate) fn expression_at_span(&self, span: Span) -> Option<u32> {
        self.span_ids().expressions.get(&span).copied()
    }

    /// The raw id of the last statement at `span`.
    pub(crate) fn statement_at_span(&self, span: Span) -> Option<u32> {
        self.span_ids().statements.get(&span).copied()
    }

    /// Drop the index after the span tables are replaced.
    pub(crate) fn forget_span_index(&mut self) {
        self.span_index = SpanIndex::default();
    }
}
