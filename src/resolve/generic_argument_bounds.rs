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
    pub(super) contradictions: Option<ArgumentContradictions>,
}

/// The arguments a generic call's constraint system rejected, ready to report in kotlinc's
/// order: `cannot infer type for type parameter` at the callee for each formal the rejections
/// leave without evidence, then one mismatch per rejected argument, in argument order.
pub(super) struct ArgumentContradictions {
    /// The signature's formals, each marked whether it is left uninferred.
    formals: Vec<(crate::types::TyParamName, bool)>,
    /// kotlinc's owner wording of the formals: `fun <T : Square> keep`, `class Keep<T : Square>`.
    owner: String,
    /// Each rejected argument with the type it is reported against. A structural parameter keeps
    /// an uninferred formal unbound, which renders as `uninferred T (of ...)`.
    mismatches: Vec<(ExprId, Ty)>,
    /// The source ordinal of the first rejected argument.
    first_argument: usize,
}

impl Checker<'_> {
    /// The arguments the solver rejects for this call, with the parameter types they are reported
    /// against. `actuals` are `(source argument, parameter, type, whole array)` in source order,
    /// `parameter` indexing the signature's parameters. A formal fixed by an explicit type
    /// argument is not inferred, so its contradictions are reported by the explicit-bound path.
    pub(super) fn argument_contradictions(
        &self,
        signature: &GenericSig,
        actuals: &[(usize, usize, Ty, bool)],
        explicit_type_args: &[Ty],
        vararg_index: Option<usize>,
        args: &[ExprId],
        owner: String,
    ) -> Option<ArgumentContradictions> {
        let violations =
            crate::symbol_resolver::infer_generic_call_constraints_with_argument_ordinals(
                &self.fed_source(),
                signature,
                actuals.iter().copied(),
                vararg_index,
            )
            .bound_violations
            .into_iter()
            .filter(|violation| explicit_type_args.get(violation.formal).is_none())
            .collect::<Vec<_>>();
        ArgumentContradictions::new(signature, &violations, owner, |source| {
            let &(_, parameter, ..) = actuals.iter().find(|actual| actual.0 == source)?;
            Some((*args.get(source)?, *signature.params.get(parameter)?))
        })
    }

    pub(super) fn report_argument_contradictions(
        &mut self,
        call: ExprId,
        contradictions: ArgumentContradictions,
    ) {
        let ArgumentContradictions {
            formals,
            owner,
            mismatches,
            ..
        } = contradictions;
        for (formal, _) in formals.iter().filter(|(_, uninferred)| *uninferred) {
            self.report_uninferred_formal(call, formal);
        }
        let owners = self.publish_formal_identity_owners(
            formals
                .iter()
                .map(|(formal, uninferred)| (formal.as_str(), uninferred.then_some(false))),
            owner,
        );
        for (argument, expected) in mismatches {
            let actual = self.expr_types[argument.0 as usize];
            self.report_assignability_error(expected, actual, self.span(argument), "argument");
        }
        self.restore_type_parameter_owners(owners);
    }

    /// Report the candidate's bound failures. Returns whether they decide the call's diagnostics,
    /// so the per-argument assignability checks must not run.
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
            contradictions,
        } = site;
        if let Some(contradictions) = contradictions {
            self.report_argument_contradictions(call, contradictions);
            return true;
        }
        for (formal_index, formal) in signature.formals.iter().enumerate() {
            let Some(actual) = bindings.get(formal.as_str()).copied() else {
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

impl ArgumentContradictions {
    /// `parameter_of` maps a rejected source argument to its expression and declared parameter.
    pub(super) fn new(
        signature: &GenericSig,
        violations: &[GenericBoundViolation],
        owner: String,
        parameter_of: impl Fn(usize) -> Option<(ExprId, Ty)>,
    ) -> Option<Self> {
        if violations.is_empty() {
            return None;
        }
        let formals = signature
            .formals
            .iter()
            .enumerate()
            .map(|(index, formal)| {
                let uninferred = violations
                    .iter()
                    .any(|violation| violation.formal == index && violation.uninferred);
                (*formal, uninferred)
            })
            .collect();
        // kotlinc reports a rejected argument once, against its parameter with every formal the
        // argument contradicts substituted or left uninferred.
        let mismatches = violations
            .chunk_by(|left, right| left.argument == right.argument)
            .filter_map(|rejected| {
                let (argument, parameter) = parameter_of(rejected[0].argument)?;
                let bare = parameter.non_null().ty_param_name();
                if let Some(violation) = rejected
                    .iter()
                    .find(|violation| bare == Some(signature.formals[violation.formal].as_str()))
                {
                    return Some((argument, parameter.rewrap_nullability(violation.expected)));
                }
                let mut binding = GSigBinds::default();
                for violation in rejected.iter().filter(|violation| !violation.uninferred) {
                    binding.insert(signature.formals[violation.formal], violation.expected);
                }
                if binding.is_empty() {
                    return Some((argument, parameter));
                }
                Some((
                    argument,
                    crate::symbol_resolver::ty_subst_keep_unbound(parameter, &binding),
                ))
            })
            .collect();
        Some(Self {
            formals,
            owner,
            mismatches,
            first_argument: violations[0].argument,
        })
    }

    pub(super) fn first_argument(&self) -> usize {
        self.first_argument
    }
}
