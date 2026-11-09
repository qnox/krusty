//! Lexical callable admission through the shared syntax-aware applicability contract.

use super::*;

pub(super) struct ScoredLocalCall {
    pub(super) rank: (usize, std::cmp::Reverse<usize>, bool),
    pub(super) signature: Signature,
}

pub(super) fn score(
    checker: &Checker<'_>,
    scope: &CheckerScope<'_>,
    signature: &Signature,
    args: &[ExprId],
    arg_tys: &[Ty],
    arg_names: Option<&[Option<String>]>,
    trailing_lambda: bool,
) -> Option<ScoredLocalCall> {
    let context_count = signature.context_count.min(signature.params.len());
    let value_parameters = (context_count..signature.params.len()).collect::<Vec<_>>();
    let mut call_sig = signature.call_sig().select_parameters(&value_parameters);
    call_sig.required = signature.required.saturating_sub(context_count);
    let mapping = call_argument_parameter_indices(
        args.len(),
        value_parameters.len(),
        arg_names,
        trailing_lambda,
        &call_sig,
    )?;
    let selected = if let Some(generic) = signature.generic_sig.as_ref() {
        let source = checker.fed_source();
        let whole_arrays = named_whole_array_varargs(&mapping, arg_names, &call_sig);
        let mut bindings =
            crate::symbol_resolver::infer_generic_call_bindings_with_receiver_from_symbols(
                &source,
                generic,
                signature.source_receiver,
                mapping
                    .iter()
                    .copied()
                    .zip(args.iter().copied())
                    .zip(arg_tys.iter().copied())
                    .zip(whole_arrays)
                    .filter_map(|(((parameter, argument), actual), whole_array)| {
                        if !call_sig.parameter_contributes_to_inference(parameter) {
                            return None;
                        }
                        let parameter = parameter + context_count;
                        let declared = *generic.params.get(parameter)?;
                        let actual = checker.call_arg_kind_with_type(scope, argument, actual);
                        (!actual.is_expected_type_callable()).then(|| {
                            (
                                parameter,
                                actual.inference_type(&source, declared),
                                whole_array || checker.file.is_spread_arg(argument),
                            )
                        })
                    }),
                signature.vararg_index,
            );
        // Captured lexical formals already belong to the enclosing application. Only the
        // selected declaration's own formals may be inferred from this argument list.
        bindings.retain(|formal, _| generic.formals.contains(formal));
        if !crate::symbol_resolver::generic_bindings_satisfy_bounds(
            generic,
            &bindings,
            |actual, bound| checker.generic_bound_admits(actual, bound),
        ) {
            return None;
        }
        local_extension_applicability::specialize_local_signature(
            &source, signature, generic, &bindings,
        )
    } else {
        signature.clone()
    };
    if let (Some(actual), Some(expected)) = (signature.source_receiver, selected.source_receiver) {
        if !checker.receiver_is_assignable(actual, expected) {
            return None;
        }
    }
    let partial_arg_tys = arg_tys.iter().copied().map(Some).collect::<Vec<_>>();
    let score = call_applicability::score(
        checker,
        scope,
        &selected.params[context_count..],
        &call_sig,
        call_applicability::ArgSlots {
            args,
            partial_arg_tys: &partial_arg_tys,
            arg_names,
            trailing_lambda,
        },
    )?;
    Some(ScoredLocalCall {
        rank: score.rank,
        signature: selected,
    })
}
