//! Declared type signatures of decoded Kotlin declarations, in semantic types.

use std::collections::HashMap;

use crate::libraries::GenericSig;
use crate::metadata::semantic::{
    semantic_bounds, semantic_ty, KotlinFunction, KotlinProperty, KotlinType, KotlinTypeParameter,
};

/// A top-level function's declared signature: its own type parameters with their bounds, the
/// extension receiver, every parameter (context parameters first), and the result. No target
/// erasure is applied; a target derives its physical shape from this.
pub(crate) fn function_generic_sig(function: &KotlinFunction) -> GenericSig {
    declared_generic_sig(
        &function.formals,
        function.receiver.as_ref(),
        &function.params,
        &function.ret,
    )
}

/// A top-level property's declared signature, in the shape of its getter: its own type parameters
/// with their bounds, the extension receiver, its context parameters, and the property type.
pub(crate) fn property_generic_sig(property: &KotlinProperty) -> GenericSig {
    declared_generic_sig(
        &property.formals,
        property.receiver.as_ref(),
        &property.context_params,
        &property.ty,
    )
}

fn declared_generic_sig(
    formals: &[KotlinTypeParameter],
    receiver: Option<&KotlinType>,
    params: &[KotlinType],
    ret: &KotlinType,
) -> GenericSig {
    let bounds = semantic_bounds(formals, &HashMap::new());
    GenericSig {
        formals: formals
            .iter()
            .map(|parameter| parameter.name.clone())
            .collect(),
        formal_bounds: formals
            .iter()
            .map(|parameter| {
                parameter
                    .bounds
                    .iter()
                    .map(|bound| semantic_ty(bound, &bounds))
                    .collect()
            })
            .collect(),
        receiver: receiver.map(|receiver| semantic_ty(receiver, &bounds)),
        params: params
            .iter()
            .map(|parameter| semantic_ty(parameter, &bounds))
            .collect(),
        ret: semantic_ty(ret, &bounds),
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
