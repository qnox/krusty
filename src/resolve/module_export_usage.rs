//! kotlinc's `FirJvmModuleAccessibilityQualifiedAccessChecker`: a callable or field is declared in
//! a classifier of a platform module that does not export the classifier's package to code outside
//! every named module. The use is reported at the called or read name, as `symbol is declared in
//! module 'M' which does not export package 'p'.`; the classifier named as a qualifier or a type is
//! reported where it is written.

use super::*;
use crate::symbol_resolver::ClassifierDenial;

impl Checker<'_> {
    /// Report the unexported declaration expression `expression` selected.
    pub(super) fn check_expression_module_export(&mut self, expression: ExprId) {
        if self.discover_anonymous_captures {
            return;
        }
        let (span, owner) = match self.file.expr(expression) {
            Expr::Call { callee, .. } => (
                match self.file.expr(*callee) {
                    Expr::Member { name, .. } => self.member_name_span(*callee, name),
                    _ => self.span(*callee),
                },
                self.resolved_constructors
                    .get(&expression)
                    .map(ResolvedConstructor::owner)
                    .or_else(|| self.resolved_calls.get(&expression)?.owner()),
            ),
            Expr::SafeCall {
                name,
                args: Some(_),
                ..
            } => (
                self.member_name_span(expression, name),
                self.resolved_calls
                    .get(&expression)
                    .and_then(ResolvedCall::owner),
            ),
            Expr::Member { name, .. } | Expr::SafeCall { name, .. } => (
                self.member_name_span(expression, name),
                self.property_read_owner(expression),
            ),
            Expr::CallableRef { name, .. } => {
                let span = self.span(expression);
                (
                    Span::new(span.hi.saturating_sub(name.len() as u32), span.hi),
                    self.reference_owner(expression),
                )
            }
            _ => return,
        };
        let Some(owner) = owner else {
            return;
        };
        let Some(denial @ ClassifierDenial::PackageNotExported { .. }) =
            self.resolver().inaccessible_classifier_access(owner)
        else {
            return;
        };
        if self.suppresses_diagnostic(denial.suppression_name()) {
            return;
        }
        self.diags.error_with_identity(
            span,
            DiagnosticIdentity::ClassifierAccess {
                reference: span,
                classifier: owner,
            },
            inaccessible_classifier_message("", &denial),
        );
    }

    /// The classifier declaring the field or property a read selected. A read through a written
    /// classifier qualifier that lowering resolves from the qualifier itself (a Java static field, a
    /// constant, an enum entry) is declared by that classifier.
    fn property_read_owner(&self, expression: ExprId) -> Option<TypeName> {
        match self.expr_lowers.get(&expression) {
            Some(ExprLowering::ClassifierPropertyRead { owner, .. })
            | Some(ExprLowering::MemberPropertyRead { owner, .. }) => Some(*owner),
            Some(_) => None,
            None => {
                if self.bound_qualifier_segment(expression).is_some() {
                    return None;
                }
                let qualifier = self.member_qualifier(expression)?;
                match self.bound_qualifier_segment(qualifier)?.binding {
                    expression_qualifiers::QualifierBinding::Classifier(owner) => Some(owner),
                    expression_qualifiers::QualifierBinding::Package(_) => None,
                }
            }
        }
    }

    /// The classifier declaring the callable a callable reference selected.
    fn reference_owner(&self, expression: ExprId) -> Option<TypeName> {
        match self.expr_lowers.get(&expression)? {
            ExprLowering::CallableReference { target, .. }
            | ExprLowering::AdaptedCallableReference { target, .. } => match target {
                CallableReferenceTarget::Classifier(member)
                | CallableReferenceTarget::Member { member, .. } => member
                    .declaration_owner
                    .map(|owner| owner.name())
                    .or(member.owner),
                _ => None,
            },
            _ => None,
        }
    }
}
