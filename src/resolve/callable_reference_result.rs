//! Whether the result a callable reference produces fits the function type it is expected as.
//!
//! One rule decides it for every reference: the named callable's result is projected onto the
//! expected result's classifier when that result is still open (`List<String>` against
//! `Iterable<R>` binds `R` from `Iterable<String>`), keeping the callable's own nullability, and is
//! then compared with the expected result. The final reference check and the probe that ranks
//! `OverloadResolutionByLambdaReturnType` candidates share it.

use super::*;

impl<'a> Checker<'a> {
    /// The result `ret` as the expected result `expected` sees it: for an open expected result,
    /// `ret` viewed as that result's classifier with its nullability kept.
    fn projected_reference_result(&self, ret: Ty, expected: Ty) -> Option<Ty> {
        if !expected.non_null().mentions_ty_param() {
            return Some(ret);
        }
        let projected = crate::assignable::applied_supertype(self, ret, expected.non_null())?;
        Some(if ret.is_nullable() {
            Ty::nullable(projected)
        } else {
            projected
        })
    }

    pub(super) fn callable_ref_is_compatible(
        &self,
        params: &[Ty],
        ret: Ty,
        suspend: bool,
        expected: &'static crate::types::FnSig,
        allow_unit_coercion: bool,
    ) -> bool {
        let ret = self
            .projected_reference_result(ret, expected.ret)
            .unwrap_or(ret);
        callable_reference_selection::is_compatible(
            params,
            ret,
            suspend,
            expected,
            allow_unit_coercion,
            |actual, target| self.receiver_is_assignable(actual, target),
        )
    }

    /// Whether the result `ret` fits the expected function type's result.
    fn reference_result_fits(&self, ret: Ty, expected: &'static crate::types::FnSig) -> bool {
        if expected.ret == Ty::Unit || expected.ret.non_null().is_ty_param() {
            return true;
        }
        let Some(projected) = self.projected_reference_result(ret, expected.ret) else {
            return false;
        };
        // An open result is still being bound: its classifier decides, and only the callable's
        // nullable result is refused by a non-null one.
        if expected.ret.non_null().mentions_ty_param() {
            return !projected.is_nullable() || expected.ret.is_nullable();
        }
        self.receiver_is_assignable(projected, expected.ret)
    }

    /// Whether some declaration the receiver-less reference `name` can name takes the expected
    /// parameters and produces a result the expected function type accepts. Parameters bind the
    /// declaration's type variables; the probe commits nothing. `None` when the name denotes no
    /// top-level or local function.
    pub(super) fn unqualified_callable_reference_result_fits(
        &self,
        scope: &CheckerScope<'_>,
        name: &str,
        expected: &'static crate::types::FnSig,
    ) -> Option<bool> {
        if let Some(overloads) = self.lookup_local_fun_overloads(scope, name) {
            return Some(overloads.into_iter().any(|(_, signature)| {
                if signature.source_receiver.is_some() {
                    return false;
                }
                let Some((params, ret)) = self.contextual_local_function_reference_shape(
                    &signature,
                    None,
                    &expected.params,
                    expected.ret,
                ) else {
                    return false;
                };
                let call_sig = CallSig {
                    param_defaults: signature.param_defaults,
                    required: signature.required,
                    vararg: signature.vararg(),
                    vararg_index: signature.vararg_index,
                    ..CallSig::default()
                };
                self.callable_ref_parameter_plan(&params, &call_sig, &expected.params)
                    .is_some()
                    && self.reference_result_fits(ret, expected)
            }));
        }
        let candidates = self.resolver().top_level_candidates(name);
        if candidates.is_empty() {
            return None;
        }
        Some(candidates.into_iter().any(|mut function| {
            function.kind == crate::libraries::FnKind::TopLevel
                && self
                    .specialize_callable_ref_candidate(
                        &mut function,
                        None,
                        Some(&expected.params),
                        None,
                    )
                    .is_ok()
                && self
                    .callable_ref_argument_plan(&function, expected)
                    .is_some()
                && self.reference_result_fits(function.callable.ret, expected)
        }))
    }
}
