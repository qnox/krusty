//! Receiverless lexical local-function references.
//!
//! Lexical collection remains scope-owned. Contextual adaptation and overload ranking reuse the
//! same provider-neutral selector as normalized member, top-level, dependency, and constructor
//! candidates; only recording the selected local declaration is specific to this module.

use super::*;

impl Checker<'_> {
    pub(super) fn select_receiverless_local_reference(
        &mut self,
        scope: &CheckerScope<'_>,
        expression: ExprId,
        name: &str,
        expected: Option<Ty>,
    ) -> Option<Ty> {
        if let Some(Ty::Fun(expected)) = expected {
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
        self.mark_local_function_ref(expression, statement, false);
        let (params, ret) = Self::local_function_reference_shape(&signature, None);
        Some(Ty::fun(params, ret))
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
                    param_defaults: signature.param_defaults.clone(),
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
                    ret: candidate.2,
                    plan: &candidate.3,
                    is_vararg: candidate.5,
                },
            )
            .collect::<Vec<_>>();
        let maximal = callable_reference_selection::maximal_adapted_references(
            &specificity,
            |left_params, left_ret, right_params, right_ret| {
                self.callable_ref_shape_at_least_as_specific(
                    left_params,
                    left_ret,
                    right_params,
                    right_ret,
                )
            },
        );
        if let [selected] = maximal.as_slice() {
            let (statement, _, _, plan, suspend_conversion, _) = &candidates[*selected];
            if Self::adapted_ref_plan_is_identity(plan) && !suspend_conversion {
                self.mark_local_function_ref(expression, *statement, false);
            } else {
                self.expr_lowers.insert(
                    expression,
                    ExprLowering::AdaptedLocalFunctionRef {
                        stmt_id: *statement,
                        bound_receiver: false,
                        argument_mapping: plan.clone(),
                        signature: Ty::Fun(expected),
                        suspend_conversion: *suspend_conversion,
                    },
                );
            }
            return Ty::Fun(expected);
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
