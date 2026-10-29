//! Bound diagnostics for a generic call whose single candidate rejected its arguments.

use super::*;
use crate::libraries::GenericSig;
use crate::symbol_resolver::{GSigBinds, GenericBoundViolation};

/// One rejected generic candidate's argument evidence, as mapped by the shared argument mapper.
pub(super) struct GenericArgumentBounds<'a> {
    pub(super) call: ExprId,
    pub(super) name: &'a str,
    pub(super) args: &'a [ExprId],
    /// Source argument to value-parameter index, before context parameters are counted.
    pub(super) parameters: Option<&'a [usize]>,
    pub(super) context_count: usize,
    pub(super) explicit_type_args: &'a [Ty],
    pub(super) signature: &'a GenericSig,
    pub(super) bindings: &'a GSigBinds,
    pub(super) contradiction: Option<GenericBoundViolation>,
}

impl Checker<'_> {
    /// The solver's argument contradiction, when it lands on a parameter that is the bare
    /// contradicted formal. Such a parameter receives the contradiction as its own mismatch; a
    /// structural parameter (`List<T>`) is reported against its substituted type instead.
    /// `actuals` are `(source argument, parameter, type, whole array)` in source order.
    pub(super) fn bare_formal_argument_contradiction(
        &self,
        signature: &GenericSig,
        actuals: &[(usize, usize, Ty, bool)],
        explicit_type_args: &[Ty],
        vararg_index: Option<usize>,
    ) -> Option<GenericBoundViolation> {
        crate::symbol_resolver::infer_generic_call_constraints_with_argument_ordinals(
            &self.fed_source(),
            signature,
            actuals.iter().copied(),
            vararg_index,
        )
        .bound_violation
        .filter(|violation| explicit_type_args.get(violation.formal).is_none())
        .filter(|violation| {
            let formal = &signature.formals[violation.formal];
            actuals
                .get(violation.argument)
                .is_some_and(|&(_, parameter, ..)| {
                    signature
                        .params
                        .get(parameter)
                        .and_then(|parameter| parameter.non_null().ty_param_name())
                        .is_some_and(|name| name == formal)
                })
        })
    }

    /// Report the candidate's bound failures. Returns whether they decide the call's diagnostics,
    /// so the per-argument assignability checks must not run.
    ///
    /// kotlinc rejects a contradicting argument's constraint at that argument. When it was the
    /// formal's only evidence, nothing is left to infer the formal from, and that is reported
    /// first, at the callee.
    pub(super) fn report_generic_argument_bounds(
        &mut self,
        site: GenericArgumentBounds<'_>,
    ) -> bool {
        let GenericArgumentBounds {
            call,
            name,
            args,
            parameters,
            context_count,
            explicit_type_args,
            signature,
            bindings,
            contradiction,
        } = site;
        if let Some(violation) = contradiction {
            if violation.uninferred {
                self.report_uninferred_formal(call, &signature.formals[violation.formal]);
            }
            let argument = args[violation.argument];
            self.expect_assignable(
                violation.expected,
                self.expr_types[argument.0 as usize],
                self.span(argument),
                "argument",
            );
            return true;
        }
        for (formal_index, formal) in signature.formals.iter().enumerate() {
            let Some(actual) = bindings.get(formal).copied() else {
                continue;
            };
            for &bound in signature
                .formal_bounds
                .get(formal_index)
                .into_iter()
                .flatten()
            {
                let expected = crate::symbol_resolver::ty_subst(bound, bindings);
                let inferred_from_callable_reference = explicit_type_args
                    .get(formal_index)
                    .is_none()
                    && parameters.is_some_and(|parameters| {
                        parameters.iter().enumerate().any(|(source, parameter)| {
                            matches!(self.file.expr(args[source]), Expr::CallableRef { .. })
                                && signature.params.get(parameter + context_count).is_some_and(
                                    |parameter| {
                                        ty_mentions_param(*parameter, std::slice::from_ref(formal))
                                    },
                                )
                        })
                    });
                crate::trace_compiler!(
                    "resolve",
                    "call={call:?} function={name} parameter={formal} binding={actual:?} bound={expected:?} callable_reference={inferred_from_callable_reference}",
                );
                if self.receiver_is_assignable(actual, expected) {
                    continue;
                }
                if inferred_from_callable_reference {
                    self.diags.error(
                        self.span(call),
                        format!(
                            "cannot infer type for type parameter '{}'. Specify it explicitly.",
                            crate::types::type_parameter_source_name(formal)
                        ),
                    );
                    return true;
                }
                let span = parameters
                    .and_then(|parameters| {
                        parameters
                            .iter()
                            .enumerate()
                            .find_map(|(source, parameter)| {
                                signature
                                    .params
                                    .get(parameter + context_count)
                                    .and_then(|parameter| parameter.non_null().ty_param_name())
                                    .is_some_and(|name| name == formal)
                                    .then(|| self.span(args[source]))
                            })
                    })
                    .unwrap_or_else(|| self.span(call));
                self.expect_assignable(expected, actual, span, "type argument");
            }
        }
        false
    }

    /// kotlinc's error for a formal left with no constraint to infer it from.
    pub(super) fn report_uninferred_formal(&mut self, call: ExprId, formal: &str) {
        self.diags.error(
            self.call_callee_name_span(call),
            format!(
                "cannot infer type for type parameter '{}'. Specify it explicitly.",
                crate::types::type_parameter_source_name(formal)
            ),
        );
    }
}
