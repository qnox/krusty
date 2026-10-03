//! Read-only callable-reference applicability while an enclosing overload is still being ranked.
//!
//! The probe reuses authoritative member and local-extension selection. It never records a target;
//! the selected-reference pass repeats the same applicability decision and owns lowering state.

use super::*;

impl Checker<'_> {
    pub(super) fn receiver_qualified_callable_reference_adapts_to(
        &self,
        scope: &CheckerScope<'_>,
        receiver: ExprId,
        name: &str,
        expected: &'static crate::types::FnSig,
    ) -> Option<bool> {
        if let Some(result) =
            self.classifier_callable_reference_adapts_to(scope, receiver, name, expected)
        {
            return Some(result);
        }

        let receiver_ty = self.expr_types[receiver.0 as usize];
        if receiver_ty == Ty::Error {
            return None;
        }
        let candidates = self.callable_ref_candidates(receiver_ty, name);
        if self
            .select_bound_callable_ref(&candidates, Some(Ty::Fun(expected)))
            .is_some()
        {
            return Some(true);
        }
        let extension_receiver = self
            .expression_function_type(scope, receiver, receiver_ty)
            .unwrap_or(receiver_ty);
        Some(self.local_extension_reference_adapts_to(
            scope,
            name,
            extension_receiver,
            expected,
            false,
        ))
    }
}
