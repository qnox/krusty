//! Conversion of compact signature result constraints into the resolver-owned representation.

pub(super) fn signature_call_result_constraint(
    constraint: Option<crate::fir::SignatureCallResultConstraint>,
) -> crate::resolve::CallResultConstraint {
    match constraint {
        None => crate::resolve::CallResultConstraint::None,
        Some(crate::fir::SignatureCallResultConstraint::Direct(expected)) => {
            crate::resolve::CallResultConstraint::Direct(expected.get())
        }
        Some(crate::fir::SignatureCallResultConstraint::SafeLifted(expected)) => {
            crate::resolve::CallResultConstraint::SafeLifted(expected.get())
        }
    }
}
