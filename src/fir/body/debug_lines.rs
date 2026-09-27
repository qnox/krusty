//! Line-only source metadata carried through consuming FIR lowering.
//!
//! These values are output facts, not source locators: they name lines, and cannot be used to
//! recover text or reparse a body. They are attached once, after checking, and travel with the body
//! into lowering so a backend can record source positions without access to the source itself.

use std::collections::HashMap;

use crate::diag::Span;

use super::*;

/// Read one parsed file's expression and statement line maps, keyed by the spans checked FIR
/// carries.
///
/// Both maps are the same responsibility — turning the parser's per-node line arrays into the
/// span-keyed form a checked body is given — and they live beside the records they build rather
/// than in the checking entry point, which owns neither.
pub(crate) fn of_file(
    file: &crate::ast::File,
) -> (
    HashMap<Span, FirExpressionDebugLines>,
    HashMap<Span, FirStatementDebugLines>,
) {
    let expressions = file
        .expr_spans
        .iter()
        .copied()
        .enumerate()
        .map(|(raw, span)| {
            (
                span,
                FirExpressionDebugLines {
                    source: file.expr_source_lines.get(raw).copied().unwrap_or(0),
                    end: file.expr_end_lines.get(raw).copied().unwrap_or(0),
                },
            )
        })
        .collect();
    let statements = file
        .stmt_spans
        .iter()
        .copied()
        .enumerate()
        .map(|(raw, span)| {
            (
                span,
                FirStatementDebugLines {
                    source: file.stmt_lines.get(raw).copied().unwrap_or(0),
                    // An assignment's lvalue line. A member assignment is a STATEMENT, whose only
                    // line is its first, so the accessor dispatch has no line of its own to restore
                    // unless the lvalue's travels with it.
                    target: file
                        .assignment_target_lines
                        .get(&(raw as u32))
                        .copied()
                        .unwrap_or(0),
                },
            )
        })
        .collect();
    (expressions, statements)
}

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

/// A body's line-only source metadata: one entry per expression and statement, the file's line
/// count, and a source lambda's closing line.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(super) struct FirBodyDebugLines {
    /// Physical source-line count for debug output; it carries no source lookup capability.
    source_line_count: u32,
    expressions: Vec<FirExpressionDebugLines>,
    statements: Vec<FirStatementDebugLines>,
    /// 1-based line of a source lambda's closing `}` (0 = unknown).
    close_line: u32,
}

impl FirBodyDebugLines {
    pub(super) fn add_expression(&mut self) {
        self.expressions.push(FirExpressionDebugLines::default());
    }

    pub(super) fn add_statement(&mut self) {
        self.statements.push(FirStatementDebugLines::default());
    }

    pub(super) fn payload_bytes(&self) -> usize {
        self.expressions.len() * std::mem::size_of::<FirExpressionDebugLines>()
            + self.statements.len() * std::mem::size_of::<FirStatementDebugLines>()
    }
}

impl FirBody {
    pub fn expression_debug_lines(&self, expression: FirExprId) -> FirExpressionDebugLines {
        self.debug_lines
            .expressions
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
        self.debug_lines
            .statements
            .get(statement.raw() as usize)
            .copied()
            .unwrap_or_default()
    }

    pub const fn source_line_count(&self) -> u32 {
        self.debug_lines.source_line_count
    }

    /// The line of a source lambda's closing `}`, where kotlinc maps its implicit `Unit` return.
    pub fn close_line(&self) -> Option<u32> {
        (self.debug_lines.close_line != 0).then_some(self.debug_lines.close_line)
    }

    pub(crate) fn attach_debug_lines(
        &mut self,
        source: SourceFileId,
        source_line_count: u32,
        origins: &OriginStore,
        expression_lines: &HashMap<Span, FirExpressionDebugLines>,
        statement_lines: &HashMap<Span, FirStatementDebugLines>,
    ) {
        self.debug_lines.source_line_count = source_line_count;
        let source_span = |origin| {
            let mut current = origin;
            loop {
                match origins.get(current)? {
                    Origin::Source { file, span } => return (file == source).then_some(span),
                    Origin::Synthetic { cause, .. } => current = cause,
                }
            }
        };
        self.debug_lines.expressions = self
            .expressions
            .iter()
            .map(|expression| {
                source_span(expression.origin)
                    .and_then(|span| expression_lines.get(&span).copied())
                    .unwrap_or_default()
            })
            .collect();
        self.debug_lines.statements = self
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
            let lambda_span = source_span(expression.origin);
            if let FirExprKind::Lambda { body, .. } = &mut expression.kind {
                // The literal's last line is its closing `}`.
                if let Some(lines) = lambda_span.and_then(|span| expression_lines.get(&span)) {
                    body.debug_lines.close_line = lines.end;
                }
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
