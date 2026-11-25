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
            .select_bound_callable_ref(&candidates, Some(Ty::from(TyKind::Fun(expected))))
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

impl Checker<'_> {
    /// Applicability of a receiver-less reference that names no local function. Selection walks the
    /// implicit receiver tower before the top-level family, so a member or extension of an
    /// implicit receiver (`::add` inside a builder lambda) is a candidate here exactly as it is
    /// when the reference is selected.
    pub(super) fn scope_resolved_callable_reference_adapts_to(
        &self,
        scope: &CheckerScope<'_>,
        name: &str,
        expected: &'static crate::types::FnSig,
    ) -> bool {
        let expectation = Ty::from(TyKind::Fun(expected));
        let implicit_receiver_fits = self.implicit_rungs(scope).into_iter().any(|rung| {
            let implicit_rungs::ImplicitRung::Receiver(receiver) = rung else {
                return false;
            };
            if receiver
                .receiver_role
                .is_some_and(ReceiverDeclarationRole::is_context_parameter)
            {
                return false;
            }
            let candidates = self.callable_ref_candidates(receiver.ty, name);
            self.select_bound_callable_ref(&candidates, Some(expectation))
                .is_some()
        });
        implicit_receiver_fits
            || self
                .resolver()
                .top_level_candidates(name)
                .into_iter()
                .any(|mut function| {
                    function.kind == crate::libraries::FnKind::TopLevel
                        && self
                            .specialize_callable_ref_candidate(
                                &mut function,
                                None,
                                Some(&expected.params),
                                Some(expected.ret),
                                None,
                            )
                            .is_ok()
                        && self
                            .callable_ref_argument_plan(&function, expected)
                            .is_some()
                        && self.callable_ref_is_compatible(
                            &expected.params,
                            function.callable.ret,
                            function.callable.suspend,
                            expected,
                            true,
                        )
                })
    }
}
