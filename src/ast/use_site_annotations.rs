//! Annotations written on an executable statement or an expression.
//!
//! Neither has a codegen representation, but both open a lexical policy for what they annotate:
//! `@Suppress` silences named diagnostics, and an opt-in marker or `@OptIn(Marker::class)` accepts
//! that marker's requirement. The parser keeps only the arguments those policies read, as source
//! spellings for the checker to resolve in the annotated element's scope, so this sidecar never
//! points into an expression arena that a compacted or released body no longer has.

use super::{AnnotationRef, Expr, ExprId, File};

/// One annotation on a statement or an expression.
#[derive(Clone, Debug)]
pub struct UseSiteAnnotation {
    pub annotation: AnnotationRef,
    /// The constant string arguments, in source order.
    pub strings: Vec<String>,
    /// The receiver spellings of the class-literal arguments (`Marker` in `Marker::class`), in
    /// source order. They are lookup input, resolved in the annotated element's scope.
    pub class_literals: Vec<String>,
}

impl File {
    pub(crate) fn use_site_annotation(
        &self,
        annotation: AnnotationRef,
        arguments: &[ExprId],
    ) -> UseSiteAnnotation {
        UseSiteAnnotation {
            annotation,
            strings: arguments
                .iter()
                .filter_map(|argument| self.const_string_value(*argument))
                .map(|value| value.to_lossy())
                .collect(),
            class_literals: self.class_literal_spellings(arguments),
        }
    }

    /// The receiver spellings of the class literals among annotation `arguments`, looking into
    /// array arguments (`[A::class, B::class]`).
    pub(crate) fn class_literal_spellings(&self, arguments: &[ExprId]) -> Vec<String> {
        let mut spellings = Vec::new();
        for &argument in arguments {
            self.collect_class_literal_spellings(argument, &mut spellings);
        }
        spellings
    }

    fn collect_class_literal_spellings(&self, argument: ExprId, spellings: &mut Vec<String>) {
        match self.expr(argument) {
            Expr::CallableRef {
                receiver: Some(receiver),
                name,
            } if name == "class" => spellings.extend(self.qualified_spelling(*receiver)),
            Expr::AnnotationArrayLiteral(elements) => {
                for &element in elements {
                    self.collect_class_literal_spellings(element, spellings);
                }
            }
            Expr::Call { args, .. } => {
                // `arrayOf(A::class)` is the call form of an array argument.
                for &element in args {
                    self.collect_class_literal_spellings(element, spellings);
                }
            }
            _ => {}
        }
    }

    /// `a.b.C` for a dotted chain of plain names, the form a class-literal receiver takes.
    fn qualified_spelling(&self, expression: ExprId) -> Option<String> {
        match self.expr(expression) {
            Expr::Name(name) => Some(name.clone()),
            Expr::Member { receiver, name } => {
                let mut qualifier = self.qualified_spelling(*receiver)?;
                qualifier.push('.');
                qualifier.push_str(name);
                Some(qualifier)
            }
            _ => None,
        }
    }
}
