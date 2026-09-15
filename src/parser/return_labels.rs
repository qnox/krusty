//! Where a `return@label`'s `@` token is, and which node it belongs to.
//!
//! The reference compiler reports an unresolvable label AT the `@`, not at the `return` keyword. The
//! label survives on the node only as a bare name, so the span cannot be recovered after parsing and
//! has to be recorded while the token is in hand. Both spellings need it — `return@l` is a statement
//! in one position and an expression in another — so the two tables live together here rather than
//! as separate facts spread across the AST root.

use crate::ast::{ExprId, StmtId};
use crate::diag::Span;
use crate::token::TokenKind;
use std::collections::HashMap;

/// The `@` token spans of the `return@label`s in one file, by the node that carries the label.
#[derive(Default, Clone)]
pub struct ReturnLabelSpans {
    statements: HashMap<StmtId, Span>,
    expressions: HashMap<ExprId, Span>,
}

impl ReturnLabelSpans {
    pub fn record_statement(&mut self, statement: StmtId, span: Option<Span>) {
        if let Some(span) = span {
            self.statements.insert(statement, span);
        }
    }

    pub fn record_expression(&mut self, expression: ExprId, span: Option<Span>) {
        if let Some(span) = span {
            self.expressions.insert(expression, span);
        }
    }

    pub fn statement(&self, statement: StmtId) -> Option<Span> {
        self.statements.get(&statement).copied()
    }

    pub fn expression(&self, expression: ExprId) -> Option<Span> {
        self.expressions.get(&expression).copied()
    }

    /// Move both tables onto the compacted arenas. They are arena-keyed like every other side table,
    /// so a body retained past Pass-1 behind discarded syntax would otherwise read a stale id.
    pub fn remap(
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

    /// Both tables are keyed by the arenas released with the bodies, so they go with them.
    pub fn clear(&mut self) {
        self.statements.clear();
        self.expressions.clear();
    }

    /// Every key must still name a live arena slot, and every span must lie inside the source.
    pub fn integrity_error(&self, statements: usize, expressions: usize, source: usize) -> Option<String> {
        for (statement, span) in &self.statements {
            if statement.0 as usize >= statements {
                return Some(format!("return-label span keyed by dangling statement {}", statement.0));
            }
            if span.hi as usize > source || span.lo > span.hi {
                return Some(format!("return-label statement span {}..{} outside the source", span.lo, span.hi));
            }
        }
        for (expression, span) in &self.expressions {
            if expression.0 as usize >= expressions {
                return Some(format!("return-label span keyed by dangling expression {}", expression.0));
            }
            if span.hi as usize > source || span.lo > span.hi {
                return Some(format!("return-label expression span {}..{} outside the source", span.lo, span.hi));
            }
        }
        None
    }
}

impl crate::parser::Parser<'_> {
    /// `return`, `return expr`, `return@label`, `return@label expr`.
    ///
    /// The `@` token's span is captured as it is consumed and recorded against the finished
    /// statement: the label reaches the AST as a bare name, so this is the only point at which the
    /// reference compiler's diagnostic position is still knowable.
    pub(super) fn parse_return_statement(&mut self, start: crate::diag::Span) -> StmtId {
        self.bump();
        let (label, label_span) = self.parse_return_label();
        let value = if self.at(TokenKind::Newline)
            || self.at(TokenKind::RBrace)
            || self.at(TokenKind::Eof)
        {
            None
        } else {
            Some(self.parse_expr())
        };
        let statement = self.finish_stmt(crate::ast::Stmt::Return(value, label), start);
        self.file
            .return_label_spans
            .record_statement(statement, label_span);
        statement
    }

    /// The `@label` of a `return@label`, with the `@` token's own span.
    ///
    /// `return@` with no name following is accepted here and left unlabelled; the missing name is a
    /// parse-level concern reported elsewhere, and inventing a label would hide it.
    pub(super) fn parse_return_label(&mut self) -> (Option<String>, Option<Span>) {
        if !self.at(TokenKind::At) {
            return (None, None);
        }
        let span = self.tok().span;
        self.bump();
        if !self.at(TokenKind::Ident) {
            return (None, Some(span));
        }
        let label = self.text().to_string();
        self.bump();
        (Some(label), Some(span))
    }

    /// `return` in EXPRESSION position (`x ?: return@label null`).
    ///
    /// The value is optional here for a different reason than in statement position: the expression
    /// context closes on a bracket or comma as well as a newline, so the terminator set is wider.
    pub(super) fn parse_return_expression(&mut self, start: Span) -> ExprId {
        self.bump();
        let (label, label_span) = self.parse_return_label();
        let value = if matches!(
            self.kind(),
            TokenKind::Newline
                | TokenKind::RBrace
                | TokenKind::RParen
                | TokenKind::RBracket
                | TokenKind::Comma
                | TokenKind::Eof
        ) {
            None
        } else {
            Some(self.parse_expr())
        };
        let end = value
            .map(|value| self.file.expr_spans[value.0 as usize])
            .unwrap_or(start);
        let expression = self.file.add_expr(
            crate::ast::Expr::Return { value, label },
            Span::new(start.lo, end.hi),
        );
        self.file
            .return_label_spans
            .record_expression(expression, label_span);
        expression
    }
}
