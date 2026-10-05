//! Annotations written on an executable statement or an expression.
//!
//! Neither has a codegen representation, but both open a lexical policy for what they annotate:
//! `@Suppress` silences named diagnostics, and an opt-in marker or `@OptIn(Marker::class)` accepts
//! that marker's requirement. The checker resolves each application, arguments included, in the
//! annotated element's scope; the arguments are ordinary expressions of the same arena.

use super::{AnnotationRef, ExprId};

/// One annotation on a statement or an expression.
#[derive(Clone, Debug)]
pub struct UseSiteAnnotation {
    pub annotation: AnnotationRef,
    /// The argument expressions, in source order.
    pub arguments: Vec<ExprId>,
}
