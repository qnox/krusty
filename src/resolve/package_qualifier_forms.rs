//! kotlinc's `ForbidAnnotationsTypeArgumentsAndParenthesesForPackageQualifier` for a package
//! qualifier in expression position.
//!
//! - With the feature, a written segment that carries annotations or explicit type arguments
//!   (`(@Ann pkg).f()`, `pkg<Int>.Obj.x`) cannot name a package, as kotlinc's
//!   `canBePackagePartOfQualifier`: the qualifier walk fails at that segment, which is then
//!   unresolved. A segment that names a classifier is unaffected.
//! - `FirParenthesizedPackageQualifierChecker` visits the FIR qualifiers of the chain
//!   ([`expression_qualifier_tree`](super::expression_qualifier_tree)). A package qualifier that
//!   its container selects through while it is wrapped in parentheses (`(pkg).f()`, `((pkg)).f()`)
//!   is reported at those parentheses, and otherwise at the first parentheses inside it
//!   (`(pkg).sub.f()`): an error with the feature, a deprecation warning without it, and nothing
//!   in a release that predates it.
//! - Before the feature, the same search also finds type-argument lists (`pkg<Int>.sub.Obj.x`),
//!   a deprecation warning at the first one. It skips a package qualifier with type arguments or an
//!   outer-class type-argument diagnostic, which are errors of their own, and a classifier
//!   continued from a package searches its receiver instead.

use super::expression_qualifier_tree::{QualifierBinding, QualifierTree};
use super::*;

use crate::features::deprecation_cycle_messages::PARENTHESIZED_PACKAGE_QUALIFIER;

/// Whether the written qualifier segment `expression` may not bind as a package under the current
/// language settings.
pub(super) fn segment_cannot_name_a_package(file: &File, expression: ExprId) -> bool {
    file.forbid_package_qualifier_forms
        && (file.expression_annotations.contains_key(&expression)
            || file.call_type_args.contains_key(&expression.0))
}

/// A written form that the checker reports on a package qualifier.
#[derive(Clone, Copy)]
enum PackageForm {
    Parenthesized(Span),
    TypeArguments(Span),
}

impl Checker<'_> {
    /// kotlinc's `FirParenthesizedPackageQualifierChecker` over the qualifiers of `tree`.
    pub(super) fn check_package_qualifier_forms(&mut self, tree: &QualifierTree) {
        if self.file.forbid_package_qualifier_forms {
            self.report_parenthesized_package_qualifiers(tree);
        } else {
            self.report_deprecated_package_qualifier_forms(tree);
        }
    }

    /// With the feature: a package qualifier wrapped in parentheses as its container's receiver,
    /// or else the first parentheses inside it.
    fn report_parenthesized_package_qualifiers(&mut self, tree: &QualifierTree) {
        for index in tree.retained().into_iter().rev() {
            let node = tree.nodes[index];
            if !matches!(node.binding, QualifierBinding::Package(_)) {
                continue;
            }
            let span = self.parenthesized_receiver(tree, index).or_else(|| {
                match self.first_package_form(node.expression, false) {
                    Some(PackageForm::Parenthesized(span)) => Some(span),
                    Some(PackageForm::TypeArguments(_)) | None => None,
                }
            });
            if let Some(span) = span {
                self.diags.error_named(
                    span,
                    crate::diag::names::PARENTHESIZED_PACKAGE_QUALIFIER_ERROR,
                    PARENTHESIZED_PACKAGE_QUALIFIER,
                );
            }
        }
    }

    /// Before the feature (`ImplementationForDeprecationWarning`): a package qualifier without
    /// type arguments of its own or of an outer segment is checked like the error, and also for a
    /// type-argument list inside it; a classifier continued from a package checks its receiver.
    fn report_deprecated_package_qualifier_forms(&mut self, tree: &QualifierTree) {
        for index in tree.retained().into_iter().rev() {
            let node = tree.nodes[index];
            let searched = match node.binding {
                QualifierBinding::Package(_) => {
                    if node.has_type_arguments || node.outer_class_type_arguments > 0 {
                        continue;
                    }
                    if let Some(span) = self.parenthesized_receiver(tree, index) {
                        self.report_package_form(PackageForm::Parenthesized(span));
                        continue;
                    }
                    node.expression
                }
                QualifierBinding::Classifier(_) => {
                    if node.has_explicit_parent || node.outer_class_type_arguments > 0 {
                        continue;
                    }
                    let Expr::Member { receiver, .. } = self.file.expr(node.expression) else {
                        continue;
                    };
                    let receiver = *receiver;
                    if let Some(span) = self.file.parenthesized_expression_spans.get(&receiver.0) {
                        self.report_package_form(PackageForm::Parenthesized(*span));
                        continue;
                    }
                    receiver
                }
            };
            if let Some(form) = self.first_package_form(searched, true) {
                self.report_package_form(form);
            }
        }
    }

    /// The parentheses around qualifier `index` when its container selects through it.
    fn parenthesized_receiver(&self, tree: &QualifierTree, index: usize) -> Option<Span> {
        if !tree.is_explicit_receiver_in_container(index) {
            return None;
        }
        self.file
            .parenthesized_expression_spans
            .get(&tree.nodes[index].expression.0)
            .copied()
    }

    /// The first parenthesized expression or, when `type_arguments`, type-argument list inside the
    /// source of `expression`, in kotlinc's pre-order (`getChild`): a receiver's parentheses before
    /// its contents, a receiver before the selector's own list. Parentheses around `expression`
    /// itself are outside its source.
    fn first_package_form(&self, expression: ExprId, type_arguments: bool) -> Option<PackageForm> {
        if let Expr::Member { receiver, .. } = self.file.expr(expression) {
            if let Some(span) = self.file.parenthesized_expression_spans.get(&receiver.0) {
                return Some(PackageForm::Parenthesized(*span));
            }
            if let Some(form) = self.first_package_form(*receiver, type_arguments) {
                return Some(form);
            }
        }
        if !type_arguments {
            return None;
        }
        self.file
            .call_type_args
            .get(&expression.0)
            .map(|arguments| PackageForm::TypeArguments(arguments.span))
    }

    fn report_package_form(&mut self, form: PackageForm) {
        let (span, name, warning) = match form {
            PackageForm::Parenthesized(span) => (
                span,
                crate::diag::names::PARENTHESIZED_PACKAGE_QUALIFIER_WARNING,
                self.file.parenthesized_package_qualifier_warning.clone(),
            ),
            PackageForm::TypeArguments(span) => (
                span,
                crate::diag::names::TYPE_ARGUMENTS_NOT_ALLOWED_IN_PACKAGE_QUALIFIER_WARNING,
                self.file.package_qualifier_type_arguments_warning.clone(),
            ),
        };
        // A release that predates the feature has neither deprecation.
        let Some(warning) = warning else {
            return;
        };
        if !self.suppresses_diagnostic(name.as_str()) {
            self.diags.warning_named(span, name, warning);
        }
    }
}
