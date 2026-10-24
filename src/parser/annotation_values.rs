//! Annotation argument grammar and the syntax context shared by nested expressions.

use super::*;

impl Parser<'_> {
    /// Parse an annotation argument list `( (name =)? value ,* )` through the real grammar. Values
    /// the checker cannot model retain an explicit [`UnsupportedAnnotationArgument`] node so the
    /// annotation-consumption site can reject them without fabricating a resolvable name.
    pub(super) fn parse_annotation_args(&mut self) -> Vec<ExprId> {
        let mut out = Vec::new();
        if !self.eat(TokenKind::LParen) {
            return out;
        }
        self.skip_newlines();
        while !self.at(TokenKind::RParen) && !self.at(TokenKind::Eof) {
            // optional named argument `name = value` — the label binds the value to an element by
            // NAME, so it is recorded rather than skipped (annotation elements have defaults and
            // may appear in any order).
            let mut argument_name = None;
            if self.at_named_arg() {
                argument_name = Some(self.text().to_string());
                self.bump(); // name
                self.bump(); // '='
            }
            if let Some(e) = self.parse_annotation_value() {
                if let Some(name) = argument_name {
                    self.file.annotation_arg_names.insert(e.0, name);
                }
                out.push(e);
            }
            self.skip_newlines();
            if !self.eat(TokenKind::Comma) {
                break;
            }
            self.skip_newlines();
        }
        self.expect(TokenKind::RParen, "')'");
        out
    }

    /// A single annotation argument value: an array literal `[…]`, a nested annotation `@Foo(…)`,
    /// or an ordinary expression (including `Foo::class`).
    fn parse_annotation_value(&mut self) -> Option<ExprId> {
        // Annotation arrays, nested annotations, and spreads recurse while a declaration prefix is
        // still being consumed, before `parse_bp` or a class-like parser is necessarily entered.
        // They are expression syntax, so join the shared expression lane instead of owning another
        // origin-specific counter/recovery policy.
        let previous = std::mem::replace(&mut self.annotation_value_context, true);
        let value = self.with_nesting_guard(
            ParserNesting::Expression,
            |_parser, _span| None,
            Self::parse_annotation_value_inner,
        );
        self.annotation_value_context = previous;
        value
    }

    fn parse_annotation_value_inner(&mut self) -> Option<ExprId> {
        if self.at(TokenKind::LBracket) {
            Some(self.parse_annotation_array_literal())
        } else if self.at(TokenKind::At) {
            let start = self.tok().span;
            let _ = self.parse_annotation();
            Some(self.file.add_expr(
                Expr::UnsupportedAnnotationArgument(
                    crate::ast::UnsupportedAnnotationArgument::NestedAnnotation,
                ),
                start,
            ))
        } else if self.at(TokenKind::Star) {
            // A spread argument in an annotation (`@A(*arrayOf("O"), "K")` — a `vararg` annotation
            // parameter). Preserve the same spread fact as an ordinary call argument; checking uses
            // it to type the whole array and constant folding flattens its elements into the vararg.
            self.bump(); // '*'
            let argument = self.parse_annotation_value()?;
            self.file.spread_arg_ids.insert(argument.0);
            Some(argument)
        } else {
            Some(self.parse_expr())
        }
    }

    pub(super) fn parse_annotation_array_literal(&mut self) -> ExprId {
        let start = self.tok().span;
        self.bump(); // '['
        self.skip_newlines();
        // Keep the annotation syntax through nested calls. The checker owns direct-slot context
        // and ordinary constructor-argument inference; neither is a parser-selected factory.
        let mut elements = Vec::new();
        while !self.at(TokenKind::RBracket) && !self.at(TokenKind::Eof) {
            if let Some(element) = self.parse_annotation_value() {
                elements.push(element);
            }
            self.skip_newlines();
            if !self.eat(TokenKind::Comma) {
                break;
            }
            self.skip_newlines();
        }
        self.expect(TokenKind::RBracket, "']'");
        self.file
            .add_expr(Expr::AnnotationArrayLiteral(elements), start)
    }
}
