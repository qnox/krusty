//! Receiverless lexical local-function references.
//!
//! Lexical collection remains scope-owned. Contextual adaptation and overload ranking reuse the
//! same provider-neutral selector as normalized member, top-level, dependency, and constructor
//! candidates; only recording the selected local declaration is specific to this module.

use super::*;
use crate::types::TyKind;

/// One receiverless local function adapted to an expected function type.
pub(super) struct AdaptedLocalReferenceShape {
    pub(super) params: Vec<Ty>,
    pub(super) ret: Ty,
    pub(super) plan: Vec<AdaptedRefArgument>,
    /// Leading context parameters the reference site binds.
    pub(super) bound_contexts: usize,
}

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
        bound_contexts: usize,
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
                bound_contexts,
                type_variable_inputs: self.active_reference_type_variable_inputs(),
                quirks: self.file.reference.quirks(),
            },
            |actual, bound| self.generic_bound_admits(actual, bound),
        )
        .ok()?;
        Some((specialized.params, specialized.ret))
    }

    /// How many leading context parameters a reference to a local function fills from its own
    /// site, exactly as for any other declaration: all of them with `CallableReferencesToContextual`,
    /// none without it.
    fn local_reference_bound_contexts(&self, signature: &Signature) -> usize {
        if self.file.language.callable_references_to_contextual {
            signature.context_count
        } else {
            0
        }
    }

    /// The adapted shape of one receiverless local function against an expected function type:
    /// its specialized parameters and result, and the plan mapping the expected inputs onto them.
    /// Every receiverless local reference probe and selection goes through this one applicability
    /// rule.
    pub(super) fn adapted_local_reference_shape(
        &self,
        signature: &Signature,
        expected: &crate::types::FnSig,
    ) -> Option<AdaptedLocalReferenceShape> {
        let bound_contexts = self.local_reference_bound_contexts(signature);
        let (params, ret) = self.contextual_local_function_reference_shape(
            signature,
            None,
            &expected.params,
            expected.ret,
            bound_contexts,
        )?;
        let call_sig = CallSig {
            param_defaults: signature.param_defaults,
            required: signature.required,
            vararg: signature.vararg(),
            vararg_index: signature.vararg_index,
            ..CallSig::default()
        };
        let plan = extension_reference_selection::checked_parameter_plan(
            &params,
            &call_sig,
            bound_contexts,
            &expected.params,
            self,
        )?;
        Some(AdaptedLocalReferenceShape {
            params,
            ret,
            plan,
            bound_contexts,
        })
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
        // Without an expected function type a reference that binds context parameters has the
        // shape of the declaration's remaining parameters, selected like any adapted reference.
        let bound_contexts = self.local_reference_bound_contexts(&signature);
        if bound_contexts > 0 && signature.generic_sig.is_none() {
            let parameters = signature.params.get(bound_contexts..)?.to_vec();
            let natural = if signature.is_suspend() {
                Ty::fun_suspend(parameters, signature.ret)
            } else {
                Ty::fun(parameters, signature.ret)
            };
            let TyKind::Fun(natural) = natural.kind() else {
                unreachable!("a function type has a function shape")
            };
            return Some(self.select_adapted_receiverless_local_reference(
                expression,
                name,
                natural,
                &[(statement, signature)],
            ));
        }
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
                let AdaptedLocalReferenceShape {
                    params,
                    ret,
                    plan,
                    bound_contexts,
                } = self.adapted_local_reference_shape(signature, expected)?;
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
                    bound_contexts,
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
            let (statement, params, ret, plan, suspend_conversion, _, bound_contexts) =
                &candidates[*selected];
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
                        bound_context_types: params[..*bound_contexts].to_vec(),
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
