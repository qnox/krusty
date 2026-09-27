//! What a bridge does with an ARGUMENT that arrives boxed where its target takes the carrier.

use super::*;
use crate::ir::Bridge;

/// Whether a bridge target's `concrete` parameter or result takes value class `X`'s carrier: `X`
/// itself, or `X?` where the carrier holds the null (a non-null reference underlying). A boxed `X?`
/// is the reference the bridge already has, and passes through as it is.
pub(super) fn carried_value_class(concrete: &Ty, under: &Under) -> Option<TypeName> {
    let classifier = concrete
        .non_null()
        .obj_internal()
        .filter(|classifier| under.contains_key(classifier))?;
    (!concrete.is_nullable() || !nullable_is_boxed(classifier, under)).then_some(classifier)
}

/// Record which of `bridge`'s arguments arrive as a boxed value class in an erased reference slot
/// and must be unboxed for the target, then erase the target's parameters to their carriers.
/// Nothing changes when no argument needs it.
pub(super) fn unbox_arguments(bridge: &mut Bridge, under: &Under) {
    let unboxed = bridge
        .concrete_params
        .iter()
        .zip(&bridge.erased_params)
        .map(|(concrete, erased)| carried_value_class(concrete, under).filter(|_| is_ref(erased)))
        .collect::<Vec<_>>();
    if unboxed.iter().all(Option::is_none) {
        return;
    }
    bridge.unbox_param_nullable = bridge
        .concrete_params
        .iter()
        .map(|parameter| parameter.is_nullable())
        .collect();
    for parameter in &mut bridge.concrete_params {
        *parameter = erase(parameter, under);
    }
    bridge.unbox_params = unboxed;
}
