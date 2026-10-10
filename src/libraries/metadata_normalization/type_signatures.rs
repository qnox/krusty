//! Declared type signatures of decoded Kotlin declarations, in semantic types.

use std::collections::HashMap;

use crate::libraries::GenericSig;
use crate::metadata::semantic::{
    semantic_bounds, semantic_ty, KotlinFunction, KotlinTypeParameter,
};

/// A top-level function's declared signature: its own type parameters with their bounds, the
/// extension receiver, every parameter (context parameters first), and the result. No target
/// erasure is applied; a target derives its physical shape from this.
pub(crate) fn function_generic_sig(function: &KotlinFunction) -> GenericSig {
    let bounds = semantic_bounds(&function.formals, &HashMap::new());
    GenericSig {
        formals: function
            .formals
            .iter()
            .map(|parameter| parameter.name.clone())
            .collect(),
        formal_bounds: function
            .formals
            .iter()
            .map(|parameter| {
                parameter
                    .bounds
                    .iter()
                    .map(|bound| semantic_ty(bound, &bounds))
                    .collect()
            })
            .collect(),
        receiver: function
            .receiver
            .as_ref()
            .map(|receiver| semantic_ty(receiver, &bounds)),
        params: function
            .params
            .iter()
            .map(|parameter| semantic_ty(parameter, &bounds))
            .collect(),
        ret: semantic_ty(&function.ret, &bounds),
        return_policy: Default::default(),
    }
}

/// Names of the type parameters metadata marks as `@OnlyInputTypes`.
pub(crate) fn only_input_type_formals(formals: &[KotlinTypeParameter]) -> Vec<String> {
    formals
        .iter()
        .filter(|parameter| parameter.only_input)
        .map(|parameter| parameter.name.clone())
        .collect()
}

/// Declaration ordinals of the type parameters metadata marks `reified`.
pub(crate) fn reified_type_parameter_ordinals(formals: &[KotlinTypeParameter]) -> Vec<u32> {
    formals
        .iter()
        .enumerate()
        .filter_map(|(ordinal, parameter)| {
            parameter
                .reified
                .then(|| u32::try_from(ordinal).ok())
                .flatten()
        })
        .collect()
}
