//! Whether an argument reaches a SAM parameter through Kotlin's function-value conversion.

use super::*;
use crate::types::TyKind;

impl Checker<'_> {
    /// Whether one expression can reach a semantic SAM parameter through Kotlin's function-value
    /// conversion. Candidate providers must share this predicate: a source constructor parameter is
    /// not a different kind of applicability boundary from an ordinary callable parameter.
    pub(super) fn sam_argument_fits(
        &self,
        scope: &CheckerScope<'_>,
        expected: Ty,
        actual: Ty,
        argument: ExprId,
    ) -> bool {
        let Some(sam) = self.semantic_sam_signature(expected) else {
            return false;
        };
        let sam_function = Ty::fun_with_shape(
            sam.params.clone(),
            sam.ret,
            sam.context_count,
            sam.has_receiver,
            sam.suspend,
        );
        if self.callable_reference_adapts_to(scope, argument, sam_function) {
            return true;
        }
        let function = self
            .expression_function_type_for_sam(
                scope,
                argument,
                actual.non_null(),
                sam.params.len(),
                sam.suspend,
            )
            .unwrap_or(actual.non_null());
        // With no written parameter list, Kotlin postpones whether a lambda is `() -> R` or
        // `(T) -> R` with implicit `it` until a candidate supplies the expected shape. The syntax
        // probe represents that state as `Error`; only SAM metadata decides whether the candidate
        // has an admissible zero/one-parameter shape.
        if function == Ty::Error {
            let checked = self.expr_types[argument.0 as usize];
            let return_matches = match checked.non_null().kind() {
                TyKind::Fun(checked) => crate::symbol_resolver::sam_return_matches(
                    self.libraries,
                    &self.fed_source(),
                    sam.ret,
                    checked.ret,
                ),
                _ => true,
            };
            return sam.params.len() <= 1 && return_matches;
        }
        (!actual.is_nullable() || expected.admits_null())
            && matches!(function.kind(), TyKind::Fun(signature)
            if signature.params.len() == sam.params.len()
                && crate::symbol_resolver::sam_return_matches(
                    self.libraries,
                    &self.fed_source(),
                    sam.ret,
                    signature.ret,
                )
                && self.written_lambda_parameters_fit(argument, signature, sam_function))
    }

    /// A lambda's written parameter types take the SAM method's parameters as a function type's
    /// do: each must be a supertype of the parameter it receives, so `{ i: Int -> i }` does not
    /// convert to `Function<Any?, Any?>` (or to `Function<*, *>`, whose method takes `Any?`).
    /// The comparison is `sam_parameters_admit`, which classpath candidate selection shares.
    fn written_lambda_parameters_fit(
        &self,
        argument: ExprId,
        lambda: &crate::types::FnSig,
        sam_function: Ty,
    ) -> bool {
        self.written_lambda_parameter_mismatch(argument, lambda, sam_function)
            .is_none()
    }

    /// The lambda's function type, with the SAM method's result, when its written parameter types
    /// do not take the SAM method's parameters.
    pub(super) fn written_lambda_parameter_mismatch(
        &self,
        argument: ExprId,
        lambda: &crate::types::FnSig,
        sam_function: Ty,
    ) -> Option<Ty> {
        let written = self
            .file
            .lambda_param_types
            .get(&argument.0)
            .is_some_and(|types| types.iter().any(Option::is_some));
        let TyKind::Fun(sam) = sam_function.kind() else {
            return None;
        };
        if !written
            || crate::symbol_resolver::sam_parameters_admit(
                &self.fed_source(),
                &sam.params,
                &lambda.params,
            )
        {
            return None;
        }
        Some(Ty::fun_with_shape(
            lambda.params.clone(),
            sam.ret,
            sam.context_count,
            sam.has_receiver,
            sam.suspend,
        ))
    }

    /// Report a lambda whose written parameter types the SAM method's parameters do not fit, as
    /// kotlinc does at the argument. Whether it was reported.
    pub(super) fn report_written_lambda_parameter_mismatch(
        &mut self,
        argument: ExprId,
        sam_function: Ty,
    ) -> bool {
        let TyKind::Fun(lambda) = self.expr_types[argument.0 as usize].non_null().kind() else {
            return false;
        };
        let Some(actual) = self.written_lambda_parameter_mismatch(argument, lambda, sam_function)
        else {
            return false;
        };
        if let Some(report) = self.argument_mismatch_message(sam_function, actual) {
            self.diags.report_error(self.span(argument), report);
        }
        true
    }
}
