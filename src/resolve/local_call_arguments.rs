//! Argument mapping and applicability of a call that names a local function.
//!
//! A local function's source arguments address its value parameters, the parameters after its
//! context parameters. Positional, named, defaulted and trailing-lambda calls all map through the
//! shared declaration-owned argument mapper, so the declared vararg slot alone decides which
//! arguments are its elements, wherever the vararg sits. An element is measured against the
//! vararg's element type; a spread or a named vararg argument passes the whole array and is
//! measured against the array. Candidates are scored by the shared call applicability, and the
//! selected call's checks consume the same declaration-owned mapping.

use crate::ast::ExprId;
use crate::libraries::CallSig;
use crate::types::Ty;

use super::call_applicability::{self, ArgSlots};
use super::{
    call_argument_parameter_indices_result, named_whole_array_varargs, Checker, CheckerScope,
    Signature,
};

/// The source arguments of a call that may select a local function.
#[derive(Clone, Copy)]
pub(super) struct LocalArguments<'a> {
    pub(super) args: &'a [ExprId],
    pub(super) names: Option<&'a [Option<String>]>,
    pub(super) trailing_lambda: bool,
}

/// How well a local candidate fits a call, as the shared call applicability ranks it.
pub(super) type LocalCandidateScore = (usize, std::cmp::Reverse<usize>, bool);

/// One source argument mapped to the value parameter that receives it.
struct MappedArgument {
    /// Index among the value parameters (after the context parameters).
    value_parameter: usize,
    /// The declared parameter type.
    declared: Ty,
    /// The argument passes the vararg's whole array (a spread, or a named vararg argument).
    whole_array: bool,
    /// The argument is one element of the vararg.
    element: bool,
}

impl MappedArgument {
    /// The type the argument is measured against.
    fn expected(&self) -> Ty {
        if self.element {
            self.declared.array_read_elem().unwrap_or(Ty::Error)
        } else {
            self.declared
        }
    }
}

/// The call shape of `signature`'s value parameters.
fn value_call_sig(signature: &Signature, context_count: usize) -> CallSig {
    let parameters = (context_count..signature.params.len()).collect::<Vec<_>>();
    signature.call_sig().select_parameters(&parameters)
}

impl Checker<'_> {
    /// Map `arguments` onto `signature`'s value parameters, one entry per source argument.
    fn map_local_arguments(
        &self,
        signature: &Signature,
        context_count: usize,
        arguments: LocalArguments<'_>,
    ) -> Result<Vec<MappedArgument>, crate::libraries::CallArgMappingFailure> {
        let call_sig = value_call_sig(signature, context_count);
        let value_parameters = call_argument_parameter_indices_result(
            arguments.args.len(),
            signature.params.len() - context_count,
            arguments.names,
            arguments.trailing_lambda,
            &call_sig,
        )?;
        let named_whole = named_whole_array_varargs(&value_parameters, arguments.names, &call_sig);
        Ok(arguments
            .args
            .iter()
            .zip(value_parameters)
            .zip(named_whole)
            .map(|((&argument, value_parameter), named_whole)| {
                let in_vararg = call_sig.vararg_index == Some(value_parameter);
                let whole_array = in_vararg && (named_whole || self.file.is_spread_arg(argument));
                MappedArgument {
                    value_parameter,
                    declared: signature.params[context_count + value_parameter],
                    whole_array,
                    element: in_vararg && !whole_array,
                }
            })
            .collect())
    }

    /// Score `signature` as a candidate for a call with `arguments` of types `arg_tys` through
    /// the shared call applicability, over its value parameters. `None` when the arguments do not
    /// map onto them or an argument does not fit.
    pub(super) fn local_function_candidate_score(
        &self,
        scope: &CheckerScope<'_>,
        signature: &Signature,
        arguments: LocalArguments<'_>,
        arg_tys: &[Ty],
    ) -> Option<LocalCandidateScore> {
        let context_count = signature.context_count.min(signature.params.len());
        let partial_arg_tys = arg_tys.iter().copied().map(Some).collect::<Vec<_>>();
        // A generic local function is measured against its parameters specialized by what the
        // arguments infer, as every other generic candidate is; an unbound formal stays a
        // formal.
        let parameters = match &signature.generic_sig {
            Some(generic) => {
                let mapped = self
                    .map_local_arguments(signature, context_count, arguments)
                    .ok()?;
                let bindings = crate::symbol_resolver::infer_generic_call_bindings_from_symbols(
                    &self.fed_source(),
                    generic,
                    mapped.iter().zip(arg_tys).map(|(argument, &actual)| {
                        (
                            context_count + argument.value_parameter,
                            actual,
                            argument.whole_array,
                        )
                    }),
                    signature.vararg_index,
                );
                generic
                    .params
                    .get(context_count..)?
                    .iter()
                    .map(|parameter| {
                        crate::symbol_resolver::ty_subst_keep_unbound(*parameter, &bindings)
                    })
                    .collect()
            }
            None => signature.params[context_count..].to_vec(),
        };
        call_applicability::score(
            self,
            scope,
            &parameters,
            &value_call_sig(signature, context_count),
            ArgSlots {
                args: arguments.args,
                partial_arg_tys: &partial_arg_tys,
                arg_names: arguments.names,
                trailing_lambda: arguments.trailing_lambda,
            },
        )
        .map(|score| score.rank)
    }

    /// Check a selected local function's arguments against `signature`, whose first
    /// `context_count` parameters are context parameters. Each argument is checked against the
    /// parameter the shared mapper assigns it, an element of the vararg against the element type
    /// and a spread or named vararg argument against the whole array, as every other selected call
    /// checks them. Returns the selected value-parameter slots: per value parameter, the source
    /// argument that fills it (a vararg's first argument), or `None` when it is omitted. Returns
    /// `None`, after reporting it, when the arguments do not map.
    pub(super) fn check_local_call_arguments(
        &mut self,
        scope: &CheckerScope<'_>,
        call: ExprId,
        arguments: LocalArguments<'_>,
        signature: &Signature,
        context_count: usize,
        function_name: &str,
    ) -> Option<Vec<Option<ExprId>>> {
        let mapped = match self.map_local_arguments(signature, context_count, arguments) {
            Ok(mapped) => mapped,
            Err(error) => {
                self.report_call_arg_mapping_error(call, arguments.args, error);
                return None;
            }
        };
        let mut slots = vec![None; signature.params.len() - context_count];
        for (&argument, mapped) in arguments.args.iter().zip(&mapped) {
            slots[mapped.value_parameter].get_or_insert(argument);
            let parameter = context_count + mapped.value_parameter;
            let expected = mapped.expected();
            let actual = self.check_argument_expected(
                scope,
                argument,
                expected,
                signature
                    .lambda_recv
                    .get(parameter)
                    .copied()
                    .unwrap_or(false),
                Some(function_name),
            );
            if mapped.whole_array {
                self.expect_whole_array_vararg_arg(Some(scope), argument, actual, expected);
            } else {
                self.expect_call_arg(scope, expected, argument, actual);
            }
        }
        Some(slots)
    }
}
