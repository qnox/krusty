//! Leading annotations on a type occurrence.
//!
//! Their classifier reference is a semantic annotation occurrence, while the following type's
//! start offset owns the ordered list. An annotation argument list must also be distinguished from
//! the parameter list of an annotated function type.

use super::*;

impl Parser<'_> {
    pub(super) fn parse_leading_type_annotations(&mut self) -> Span {
        let mut annotations = Vec::new();
        while self.at(TokenKind::At) {
            let at = self.tok().span.lo;
            self.bump(); // '@'
            let (name, annotation_span) = self.parse_annotation_reference();
            // `@Ann("a") String` owns the parentheses, while `@Composable () -> Unit` leaves them
            // for the function type. The balanced lookahead identifies the latter exact shape.
            if self.at(TokenKind::LParen) && !self.paren_group_precedes_arrow(self.i) {
                let arguments = self.parse_annotation_args();
                self.record_type_annotation_arguments(annotation_span, arguments);
            }
            if !name.is_empty() {
                annotations.push(AnnotationRef {
                    name,
                    at,
                    span: annotation_span,
                    property_site: None,
                });
            }
            // Kotlin permits newlines between the annotation and its type. A semicolon is not
            // such whitespace and remains for the ordinary type parser to reject.
            while self.at(TokenKind::Newline) && !self.at_semicolon() {
                self.bump();
            }
        }
        let span = self.tok().span;
        if !annotations.is_empty() {
            self.file.type_annotations.insert(span.lo, annotations);
        }
        span
    }
}
