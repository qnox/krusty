//! Select diagnostic facts after semantic lookup has failed.
//!
//! Lookup remains provider-driven and identity-based. This boundary decides only which facts from
//! the checked receiver affect the reference compiler's diagnostic wording.

use super::*;

/// `UNRESOLVED_REFERENCE` for a member looked up on an explicit receiver. A smart cast names the
/// narrowed type, a safe call the non-null one, a platform type its Kotlin spelling, and the null
/// literal `Nothing?`. Type parameters and failed types have no reportable receiver type.
pub(crate) fn unresolved_member_message(
    name: &str,
    receiver: Ty,
    hidden_deprecated: bool,
) -> String {
    if hidden_deprecated {
        return crate::diagnostic_wording::unresolved_reference_on(name, None);
    }
    let rendered = match receiver {
        Ty::Error | Ty::Pending | Ty::TyParam(..) | Ty::Nullable(Ty::TyParam(..)) => None,
        Ty::Null => Some("Nothing?".to_string()),
        Ty::PlatformNullable(inner) => Some(inner.source_name()),
        other => Some(other.source_name()),
    };
    crate::diagnostic_wording::unresolved_reference_on(name, rendered.as_deref())
}

impl Checker<'_> {
    /// The span selected by one versioned diagnostic positioning strategy.
    pub(super) fn anchored_span(
        &self,
        anchor: crate::diagnostic_wording::Anchor,
        call: ExprId,
        value_arguments: impl FnOnce() -> Span,
    ) -> Span {
        match anchor {
            crate::diagnostic_wording::Anchor::ValueArguments => value_arguments(),
            crate::diagnostic_wording::Anchor::ReferencedNameByQualified => {
                self.call_callee_name_span(call)
            }
        }
    }

    pub(super) fn call_callee_name_span(&self, call: ExprId) -> Span {
        let callee = match self.file.expr(call) {
            Expr::Call { callee, .. } => callee,
            Expr::SafeCall { name, .. } => return self.member_name_span(call, name),
            _ => return self.span(call),
        };
        match self.file.expr(*callee) {
            Expr::Member { name, .. } => self.member_name_span(*callee, name),
            _ => self.span(*callee),
        }
    }

    /// A classifier qualifier (`Limits.MAX` through a companion or `Obj.x`) is not a receiver value,
    /// so kotlinc does not append a receiver type to its unresolved-reference diagnostic.
    pub(super) fn unresolved_member_diagnostic(
        &self,
        scope: &CheckerScope<'_>,
        receiver: Option<ExprId>,
        name: &str,
        receiver_ty: Ty,
    ) -> String {
        let qualifier = receiver.is_some_and(|receiver| {
            matches!(
                self.qualifier(scope, QualifierInput::Expression(receiver)),
                Ok(ResolvedQualifier::Classifier(_))
            )
        });
        if qualifier {
            crate::diagnostic_wording::unresolved_reference_on(name, None)
        } else {
            unresolved_member_message(
                name,
                receiver_ty,
                self.resolver()
                    .receiver_has_hidden_deprecated_member(receiver_ty, name),
            )
        }
    }
}
