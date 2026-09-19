//! Line-only source metadata carried through consuming FIR lowering.
//!
//! These values are output facts, not source locators: they name lines, and cannot be used to
//! recover text or reparse a body. They are attached once, after checking, and travel with the body
//! into lowering so a backend can record source positions without access to the source itself.

use std::collections::HashMap;

use crate::diag::Span;

use super::*;

/// Line-only source metadata for one EXPRESSION.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FirExpressionDebugLines {
    pub source: u32,
    pub end: u32,
}

/// The same, for a STATEMENT. `target` is an assignment's lvalue line — where the member it writes
/// is named — and is 0 for every other statement form.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FirStatementDebugLines {
    pub source: u32,
    pub target: u32,
}

impl FirBody {
    pub fn expression_debug_lines(&self, expression: FirExprId) -> FirExpressionDebugLines {
        self.expression_debug_lines
            .get(expression.raw() as usize)
            .copied()
            .unwrap_or_default()
    }

    pub fn statement_debug_line(&self, statement: FirStatementId) -> u32 {
        self.statement_debug_lines(statement).source
    }

    /// The line an assignment statement's LVALUE is named on — where its write dispatches — or 0
    /// when this statement is not an assignment.
    pub fn statement_target_debug_line(&self, statement: FirStatementId) -> u32 {
        self.statement_debug_lines(statement).target
    }

    fn statement_debug_lines(&self, statement: FirStatementId) -> FirStatementDebugLines {
        self.statement_debug_lines
            .get(statement.raw() as usize)
            .copied()
            .unwrap_or_default()
    }

    pub const fn source_line_count(&self) -> u32 {
        self.source_line_count
    }

    pub(crate) fn attach_debug_lines(
        &mut self,
        source: SourceFileId,
        source_line_count: u32,
        origins: &OriginStore,
        expression_lines: &HashMap<Span, FirExpressionDebugLines>,
        statement_lines: &HashMap<Span, FirStatementDebugLines>,
    ) {
        self.source_line_count = source_line_count;
        let source_span = |origin| {
            let mut current = origin;
            loop {
                match origins.get(current)? {
                    Origin::Source { file, span } => return (file == source).then_some(span),
                    Origin::Synthetic { cause, .. } => current = cause,
                }
            }
        };
        self.expression_debug_lines = self
            .expressions
            .iter()
            .map(|expression| {
                source_span(expression.origin)
                    .and_then(|span| expression_lines.get(&span).copied())
                    .unwrap_or_default()
            })
            .collect();
        self.statement_debug_lines = self
            .statements
            .iter()
            .map(|statement| {
                source_span(statement.origin)
                    .and_then(|span| statement_lines.get(&span).copied())
                    .unwrap_or_default()
            })
            .collect();
        for statement in &mut self.statements {
            if let FirStatementKind::LocalFunction { body, .. } = &mut statement.kind {
                body.attach_debug_lines(
                    source,
                    source_line_count,
                    origins,
                    expression_lines,
                    statement_lines,
                );
            }
        }
        for expression in &mut self.expressions {
            if let FirExprKind::Lambda { body, .. } = &mut expression.kind {
                body.attach_debug_lines(
                    source,
                    source_line_count,
                    origins,
                    expression_lines,
                    statement_lines,
                );
            }
        }
        for body in &mut self.inline_nested_declaration_bodies {
            body.attach_debug_lines(
                source,
                source_line_count,
                origins,
                expression_lines,
                statement_lines,
            );
        }
    }
}
