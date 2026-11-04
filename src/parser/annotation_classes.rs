//! `annotation class` declarations. Their primary-constructor parameter defaults are annotation
//! values, so `[…]` there is an annotation array literal rather than a collection literal.

use super::*;

impl Parser<'_> {
    /// `annotation class Name(…)`, after the `annotation` modifier.
    pub(super) fn parse_annotation_class(&mut self) -> ClassDecl {
        self.annotation_class_header = true;
        let mut declaration = self.parse_class();
        declaration.kind = ClassKind::Annotation;
        declaration
    }

    /// The `= default` of a primary-constructor parameter, read as an annotation value inside an
    /// annotation class header.
    pub(super) fn primary_parameter_default(&mut self, annotation_class: bool) -> Option<ExprId> {
        if !self.eat(TokenKind::Eq) {
            return None;
        }
        self.skip_newlines();
        if annotation_class {
            self.parse_annotation_value()
        } else {
            Some(self.parse_expr())
        }
    }
}
