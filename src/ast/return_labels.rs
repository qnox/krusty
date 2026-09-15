//! Exact source spans attached to `return@label` nodes.
//!
//! The parser records the `@` token while it is in hand. The AST owns the resulting side tables:
//! their keys share the AST arenas' lifetime, compaction must remap them, release must clear them,
//! and validation checks them alongside the arenas. Later phases read the spans only to report the
//! reference compiler's diagnostic position.

use super::{ExprId, StmtId};
use crate::diag::Span;
use std::collections::HashMap;

#[derive(Default, Clone)]
pub(crate) struct ReturnLabelSpans {
    statements: HashMap<StmtId, Span>,
    expressions: HashMap<ExprId, Span>,
}

impl ReturnLabelSpans {
    pub(crate) fn record_statement(&mut self, statement: StmtId, span: Option<Span>) {
        if let Some(span) = span {
            self.statements.insert(statement, span);
        }
    }

    pub(crate) fn record_expression(&mut self, expression: ExprId, span: Option<Span>) {
        if let Some(span) = span {
            self.expressions.insert(expression, span);
        }
    }

    pub(crate) fn statement(&self, statement: StmtId) -> Option<Span> {
        self.statements.get(&statement).copied()
    }

    pub(crate) fn expression(&self, expression: ExprId) -> Option<Span> {
        self.expressions.get(&expression).copied()
    }

    /// Move both tables onto compacted arenas and drop entries for discarded syntax.
    pub(crate) fn remap(
        &mut self,
        statements: &HashMap<StmtId, StmtId>,
        expressions: &HashMap<ExprId, ExprId>,
    ) {
        self.statements = std::mem::take(&mut self.statements)
            .into_iter()
            .filter_map(|(old, span)| statements.get(&old).map(|new| (*new, span)))
            .collect();
        self.expressions = std::mem::take(&mut self.expressions)
            .into_iter()
            .filter_map(|(old, span)| expressions.get(&old).map(|new| (*new, span)))
            .collect();
    }

    pub(crate) fn clear(&mut self) {
        self.statements.clear();
        self.expressions.clear();
    }

    pub(crate) fn integrity_error(
        &self,
        statements: usize,
        expressions: usize,
        source: usize,
    ) -> Option<String> {
        for (statement, span) in &self.statements {
            if statement.0 as usize >= statements {
                return Some(format!(
                    "return-label span keyed by dangling statement {}",
                    statement.0
                ));
            }
            if span.hi as usize > source || span.lo > span.hi {
                return Some(format!(
                    "return-label statement span {}..{} outside the source",
                    span.lo, span.hi
                ));
            }
        }
        for (expression, span) in &self.expressions {
            if expression.0 as usize >= expressions {
                return Some(format!(
                    "return-label span keyed by dangling expression {}",
                    expression.0
                ));
            }
            if span.hi as usize > source || span.lo > span.hi {
                return Some(format!(
                    "return-label expression span {}..{} outside the source",
                    span.lo, span.hi
                ));
            }
        }
        None
    }
}
