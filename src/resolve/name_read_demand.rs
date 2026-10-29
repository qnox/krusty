//! Demand inferred name types only after preserving their receiver requirements.

use super::*;

pub(super) fn inferred_receiver_member(
    checker: &Checker<'_>,
    scope: &CheckerScope<'_>,
    binding: &Local,
    name: &str,
) -> Option<Ty> {
    // Dispatch properties demand through the actual selected live rung later in name checking.
    // Storage/local bindings can still carry an inferred member type on an enclosing receiver.
    if !binding.ty.mentions_pending()
        || matches!(
            binding.origin,
            ReceiverFnValueOrigin::DispatchProperty { .. }
        )
    {
        return None;
    }
    let demand = checker.demand_member?;
    checker
        .implicit_receivers(scope)
        .into_iter()
        .find_map(|receiver| {
            (receiver.ty != Ty::Error && !receiver.ty.mentions_pending())
                .then(|| demand(receiver.ty.non_null(), name, &[]))
                .flatten()
                .filter(|answered| *answered != Ty::Error && !answered.mentions_pending())
        })
}

pub(super) fn rejected_name(
    checker: &mut Checker<'_>,
    scope: &CheckerScope<'_>,
    expression: ExprId,
    name: &str,
) -> Ty {
    if !receiver_tower::reject_unavailable_member(checker, scope, expression, name) {
        let _ = checker.demand_name.and_then(|demand| demand(name));
        checker.diags.error(
            checker.span(expression),
            format!("unresolved reference '{name}'."),
        );
    }
    Ty::Error
}

pub(super) fn unresolved_name(
    checker: &mut Checker<'_>,
    scope: &CheckerScope<'_>,
    expression: ExprId,
    name: &str,
) -> Option<Ty> {
    if receiver_tower::reject_unavailable_member(checker, scope, expression, name) {
        Some(Ty::Error)
    } else {
        checker.demand_name.and_then(|demand| demand(name))
    }
}
