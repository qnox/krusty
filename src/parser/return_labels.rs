use crate::ast::{ExprId, StmtId};
use crate::diag::Span;
use crate::token::TokenKind;

impl crate::parser::Parser<'_> {
    /// `return`, `return expr`, `return@label`, `return@label expr`.
    ///
    /// The `@` token's span is captured as it is consumed and recorded against the finished
    /// statement: the label reaches the AST as a bare name, so this is the only point at which the
    /// reference compiler's diagnostic position is still knowable.
    pub(super) fn parse_return_statement(&mut self, start: crate::diag::Span) -> StmtId {
        self.bump();
        let (label, label_span) = self.parse_return_label();
        let value =
            if self.at(TokenKind::Newline) || self.at(TokenKind::RBrace) || self.at(TokenKind::Eof)
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
