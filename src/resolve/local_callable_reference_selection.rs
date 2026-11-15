//! Receiverless lexical local-function references.
//!
//! Lexical collection remains scope-owned. Contextual adaptation and overload ranking reuse the
//! same provider-neutral selector as normalized member, top-level, dependency, and constructor
//! candidates; only recording the selected local declaration is specific to this module.

use super::*;
use crate::types::TyKind;

impl Checker<'_> {
    /// Semantic callable shape of a local function reference. The lifted method signature keeps
    /// erased JVM parameter/return types in `params`/`ret`; source generic types live in
    /// `generic_sig` and must be applied to the reference receiver before the value is exposed.
    pub(super) fn local_function_reference_shape(
        signature: &Signature,
        receiver: Option<Ty>,
    ) -> (Vec<Ty>, Ty) {
        let Some(generic) = signature.generic_sig.as_ref() else {
            return (signature.params.clone(), signature.ret);
        };
        let mut bindings = crate::symbol_resolver::GSigBinds::default();
        if let Some((declared, actual)) = generic.receiver.zip(receiver) {
            crate::symbol_resolver::unify_ty(declared, actual, &mut bindings);
        }
        (
            generic
                .params
                .iter()
                .map(|parameter| {
                    crate::symbol_resolver::ty_subst_keep_unbound(*parameter, &bindings)
                })
                .collect(),
            crate::symbol_resolver::ty_subst_keep_unbound(generic.ret, &bindings),
        )
    }

    /// Apply a contextual callable shape to a generic local declaration through the same
    /// provider-neutral specialization used for module and dependency candidates. Local lifting is
    /// a lowering detail; it must not create a second inference algorithm for callable references.
    pub(super) fn contextual_local_function_reference_shape(
        &self,
        signature: &Signature,
        receiver: Option<Ty>,
        expected_params: &[Ty],
        expected_ret: Ty,
    ) -> Option<(Vec<Ty>, Ty)> {
        let Some(generic) = signature.generic_sig.as_ref() else {
            return Some(Self::local_function_reference_shape(signature, receiver));
        };
        let call_sig = CallSig {
            param_defaults: signature.param_defaults,
            required: signature.required,
            vararg: signature.vararg(),
            vararg_index: signature.vararg_index,
            ..CallSig::default()
        };
        let source = self.fed_source();
        let specialized = callable_reference_selection::specialize_generic_signature(
            &source,
            generic,
            &call_sig,
            receiver,
            callable_reference_selection::ReferenceExpectation {
                params: Some(expected_params),
                ret: Some(expected_ret),
                bound_contexts: 0,
                type_variable_inputs: self.active_reference_type_variable_inputs(),
            },
            |actual, bound| self.generic_bound_admits(actual, bound),
        )
        .ok()?;
        Some((specialized.params, specialized.ret))
    }

    pub(super) fn select_receiverless_local_reference(
        &mut self,
        scope: &CheckerScope<'_>,
        expression: ExprId,
        name: &str,
        expected: Option<Ty>,
    ) -> Option<Ty> {
        if let Some(TyKind::Fun(expected)) = expected.map(Ty::kind) {
            if let Some(overloads) = self.lookup_local_fun_overloads(scope, name) {
                let local_functions = overloads
                    .into_iter()
                    .filter(|(_, signature)| signature.source_receiver.is_none())
                    .collect::<Vec<_>>();
                if !local_functions.is_empty() {
                    return Some(self.select_adapted_receiverless_local_reference(
                        expression,
                        name,
                        expected,
                        &local_functions,
                    ));
                }
            }
        }
        let (statement, signature) = self.lookup_local_fun(scope, name)?;
        self.record_local_reference_diagnostic(expression, statement, signature.ret);
        self.mark_local_function_ref(expression, statement, false);
        let (params, ret) = Self::local_function_reference_shape(&signature, None);
        // No expected function type still keeps the declaration's suspend kind. Dropping it
        // types `::suspendLocal` as `() -> R`, so the carrier implements `Function0` and its
        // erased `invoke` loads a continuation that is not a parameter.
        Some(if signature.is_suspend() {
            Ty::fun_suspend(params, ret)
        } else {
            Ty::fun(params, ret)
        })
    }

    fn select_adapted_receiverless_local_reference(
        &mut self,
        expression: ExprId,
        name: &str,
        expected: &'static crate::types::FnSig,
        local_functions: &[(StmtId, Signature)],
    ) -> Ty {
        let candidates = local_functions
            .iter()
            .filter_map(|(statement, signature)| {
                if signature.is_suspend() && !expected.suspend {
                    return None;
                }
                let (params, ret) = self.contextual_local_function_reference_shape(
                    signature,
                    None,
                    &expected.params,
                    expected.ret,
                )?;
                let call_sig = crate::libraries::CallSig {
                    param_defaults: signature.param_defaults,
                    required: signature.required,
                    vararg: signature.vararg(),
                    vararg_index: signature.vararg_index,
                    ..crate::libraries::CallSig::default()
                };
                let plan =
                    self.callable_ref_parameter_plan(&params, &call_sig, &expected.params)?;
                if !self.callable_ref_is_compatible(
                    &expected.params,
                    ret,
                    signature.is_suspend(),
                    expected,
                    true,
                ) {
                    return None;
                }
                Some((
                    *statement,
                    params,
                    ret,
                    plan,
                    expected.suspend && !signature.is_suspend(),
                    signature.vararg_index.is_some(),
                ))
            })
            .collect::<Vec<_>>();
        let specificity = candidates
            .iter()
            .map(
                |candidate| callable_reference_selection::AdaptedReferenceSpecificity {
                    parameters: &candidate.1,
                    plan: &candidate.3,
                    is_vararg: candidate.5,
                },
            )
            .collect::<Vec<_>>();
        let maximal = callable_reference_selection::maximal_adapted_references(
            &specificity,
            |left_params, right_params| {
                self.callable_ref_parameter_shape_at_least_as_specific(left_params, right_params)
            },
        );
        if let [selected] = maximal.as_slice() {
            let (statement, params, ret, plan, suspend_conversion, _) = &candidates[*selected];
            self.record_local_reference_diagnostic(expression, *statement, *ret);
            // The expected shape may still hold the enclosing call's type variables
            // (`s.let(::local)`); the reference's type is the shape the local function realizes.
            let realized =
                callable_reference_selection::realize_expected_shape(expected, params, *ret, plan);
            if Self::adapted_ref_plan_is_identity(plan) && !suspend_conversion {
                self.mark_local_function_ref(expression, *statement, false);
            } else {
                self.expr_lowers.insert(
                    expression,
                    ExprLowering::AdaptedLocalFunctionRef {
                        stmt_id: *statement,
                        bound_receiver: false,
                        argument_mapping: plan.clone(),
                        signature: realized,
                        suspend_conversion: *suspend_conversion,
                    },
                );
            }
            return realized;
        }
        let message = if candidates.is_empty() {
            format!("none of the local function candidates for '{name}' is applicable")
        } else {
            let mut message = "overload resolution ambiguity between candidates:".to_string();
            for index in maximal {
                let Stmt::LocalFun(function) = self.file.stmt(candidates[index].0) else {
                    unreachable!("a local reference candidate must be a local function")
                };
                message.push('\n');
                message.push_str(&source_function_display(
                    self.file,
                    function,
                    candidates[index].2,
                ));
            }
            message
        };
        self.diags
            .error(self.member_name_span(expression, name), message);
        Ty::Error
    }
}
