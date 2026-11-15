//! Inverting a call's argument-to-slot mapping.
//!
//! Kotlin's named/default/vararg/trailing-lambda mapping assigns written arguments to declaration
//! slots. Inference and commitment read the inverse: the slot each written argument fills, and
//! whether a vararg argument passes the whole array.

use super::*;

pub(super) fn call_argument_parameter_indices(
    argument_count: usize,
    parameter_count: usize,
    names: Option<&[Option<String>]>,
    trailing_lambda: bool,
    sig: &CallSig,
) -> Option<Vec<usize>> {
    call_argument_parameter_indices_result(
        argument_count,
        parameter_count,
        names,
        trailing_lambda,
        sig,
    )
    .ok()
}

pub(super) fn call_argument_parameter_indices_result(
    argument_count: usize,
    parameter_count: usize,
    names: Option<&[Option<String>]>,
    trailing_lambda: bool,
    sig: &CallSig,
) -> Result<Vec<usize>, CallArgMappingFailure> {
    // Map stable source indices rather than expression/type values. Expressions may be repeated
    // syntactically and partial types deliberately contain several identical `None` entries for
    // postponed lambdas; indices make the inversion unambiguous for every call origin.
    let arguments = (0..argument_count).collect::<Vec<_>>();
    let slots = map_call_args(
        &arguments,
        names,
        &sig.param_names,
        parameter_count,
        sig.required,
        &sig.param_defaults,
        sig.vararg_index,
        trailing_lambda,
    )?;
    let indices = arguments
        .into_iter()
        .map(|argument| {
            slots
                .iter()
                .position(|slot| *slot == Some(argument))
                .or(sig.vararg_index)
                .expect("successful argument mapping must assign every argument")
        })
        .collect::<Vec<_>>();
    Ok(indices)
}

pub(super) fn named_whole_array_varargs(
    argument_map: &[usize],
    names: Option<&[Option<String>]>,
    sig: &CallSig,
) -> Vec<bool> {
    argument_map
        .iter()
        .enumerate()
        .map(|(argument, &parameter)| {
            sig.vararg_index == Some(parameter)
                && names
                    .and_then(|names| names.get(argument))
                    .is_some_and(Option::is_some)
        })
        .collect()
}
