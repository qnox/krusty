//! kotlinc's checks of type arguments written on the qualifiers of an expression qualifier chain
//! (`a.b<Int>.f()`, `Outer<Int>.Nested.f()`), over the FIR qualifiers of
//! [`expression_qualifier_tree`](super::expression_qualifier_tree):
//!
//! - `FirQualifierWithTypeArgumentsChecker`: each qualifier in the tree reports
//!   "type arguments for outer class are redundant when nested class is referenced." once per
//!   outer-class type-argument diagnostic it carries, unless it names an `inner` class.
//! - The value selection of a classifier whose predecessor has type arguments reports
//!   `NESTED_CLASS_ACCESSED_VIA_INSTANCE_REFERENCE` at the segment's name.
//! - `FirTypeArgumentsNotAllowedExpressionChecker`: a qualified access whose explicit receiver is a
//!   package with type arguments reports "type arguments are not allowed for packages." on that
//!   receiver. With `ForbidAnnotationsTypeArgumentsAndParenthesesForPackageQualifier` such a
//!   segment does not name a package at all.
//! - `FirGenericQualifierOnConstructorCallChecker`: a constructor call of a non-`inner` class
//!   through a qualifier with type arguments is "usage of a qualifier with type arguments to call
//!   nested class constructor is deprecated. The type arguments must be removed."

use super::expression_qualifier_tree::{QualifierBinding, QualifierTree};
use super::*;

const GENERIC_QUALIFIER_ON_CONSTRUCTOR_CALL: &str =
    "usage of a qualifier with type arguments to call \
     nested class constructor is deprecated. The type arguments must be removed.";
const OUTER_CLASS_TYPE_ARGUMENTS: &str =
    "type arguments for outer class are redundant when nested class is referenced.";

impl Checker<'_> {
    pub(super) fn check_outer_class_type_arguments(&mut self, tree: &QualifierTree) {
        for index in tree.retained().into_iter().rev() {
            let node = tree.nodes[index];
            let inner = match node.binding {
                QualifierBinding::Classifier(internal) => self
                    .resolver()
                    .classifier(internal)
                    .is_some_and(|classifier| classifier.outer_instance.is_some()),
                QualifierBinding::Package(_) => false,
            };
            if inner {
                continue;
            }
            let span = self.qualifier_node_span(node.expression);
            for _ in 0..node.outer_class_type_arguments {
                self.diags
                    .error(span, OUTER_CLASS_TYPE_ARGUMENTS.to_string());
            }
        }
        let (Some(value), [.., previous, last]) = (tree.value, tree.nodes.as_slice()) else {
            return;
        };
        if !previous.has_type_arguments {
            return;
        }
        let (QualifierBinding::Classifier(internal), Expr::Member { name, .. }) =
            (last.binding, self.file.expr(last.expression))
        else {
            return;
        };
        let rendered = if value.alias {
            format!("typealias '{name}'")
        } else {
            let kind = self.classifier_kind_word(previous.binding, internal);
            format!("{kind} '{}'", internal.nested_segment_ref())
        };
        let span = self.member_name_span(last.expression, name);
        self.diags.error(
            span,
            format!("nested {rendered} accessed via instance reference."),
        );
    }

    /// `FirGenericQualifierOnConstructorCallChecker`: a call that selected the constructor of a
    /// class that is not `inner` through an explicit receiver qualifier with FIR type arguments
    /// (`pkg.sub<Int>.Cls()`, `pkg.Outer<Int>.Nested.Deep()`) reports
    /// `GENERIC_QUALIFIER_ON_CONSTRUCTOR_CALL` at that qualifier's type-argument list: its own list,
    /// or the qualifier's start when the arguments are inherited from an outer classifier.
    pub(super) fn check_generic_qualifier_on_constructor_call(
        &mut self,
        tree: &QualifierTree,
        call: ExprId,
    ) {
        let last = tree.last();
        if !last.has_type_arguments {
            return;
        }
        let Some(constructed) = self.resolved_constructors.get(&call).map(|c| c.owner()) else {
            return;
        };
        let inner = self
            .resolver()
            .classifier(constructed)
            .is_none_or(|classifier| classifier.outer_instance.is_some());
        if inner {
            return;
        }
        let span = self
            .file
            .call_type_args
            .get(&last.expression.0)
            .map(|arguments| arguments.span)
            .unwrap_or_else(|| self.qualifier_node_span(last.expression));
        self.diags
            .error(span, GENERIC_QUALIFIER_ON_CONSTRUCTOR_CALL.to_string());
    }

    pub(super) fn check_package_receiver_type_arguments(&mut self, tree: &QualifierTree) {
        let last = tree.last();
        if self.file.forbid_package_qualifier_forms
            || !last.has_type_arguments
            || !matches!(last.binding, QualifierBinding::Package(_))
        {
            return;
        }
        let span = self.qualifier_node_span(last.expression);
        self.diags
            .error(span, package_qualifier_forms::TYPE_ARGUMENTS.to_string());
    }

    /// kotlinc's class-or-object word for a classifier selected through `owner`.
    fn classifier_kind_word(&self, owner: QualifierBinding, internal: TypeName) -> &'static str {
        let resolver = self.resolver();
        let companion = match owner {
            QualifierBinding::Classifier(owner) => resolver
                .classifier(owner)
                .and_then(|owner| owner.companion_object.clone())
                .is_some_and(|(_, companion)| companion == internal),
            QualifierBinding::Package(_) => false,
        };
        if companion {
            return "companion object";
        }
        match resolver
            .classifier(internal)
            .map(|classifier| classifier.kind)
        {
            Some(crate::libraries::TypeKind::Interface) => "interface",
            Some(crate::libraries::TypeKind::Enum) => "enum class",
            _ => "class",
        }
    }

    /// The source of the qualifier ending at `expression`: the written chain from its root,
    /// including parentheses around a receiver but not around the qualifier itself.
    pub(super) fn qualifier_node_span(&self, expression: ExprId) -> Span {
        let span = self.span(expression);
        let mut start = expression;
        while let Expr::Member { receiver, .. } = self.file.expr(start) {
            if let Some(parenthesized) = self.file.parenthesized_expression_spans.get(&receiver.0) {
                return Span::new(parenthesized.lo, span.hi);
            }
            start = *receiver;
        }
        Span::new(self.span(start).lo, span.hi)
    }
}
