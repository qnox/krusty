//! Stable provider projection for value-class adaptation of inherited default calls.

use crate::fir::{CallableId, ResolvedFunctionOverrideTarget};
use crate::ir::IrFile;
use crate::types::Ty;

pub(super) fn module_provider(
    target: CallableId,
    provider: ResolvedFunctionOverrideTarget,
) -> CallableId {
    let ResolvedFunctionOverrideTarget::Module(provider) = provider else {
        panic!("external inherited-default call for {target:?} survived JVM realization");
    };
    provider
}

pub(super) fn supplied_provider_parameters<'a>(
    ir: &'a IrFile,
    target: CallableId,
    provider: ResolvedFunctionOverrideTarget,
    omitted: &'a [u32],
) -> impl Iterator<Item = Ty> + 'a {
    let provider = module_provider(target, provider);
    let callable = ir
        .referenced_module_callables
        .get(&provider)
        .expect("an inherited module default provider must retain its IR callable");
    callable
        .parameters
        .iter()
        .enumerate()
        .filter_map(move |(ordinal, ty)| (!omitted.contains(&(ordinal as u32))).then_some(*ty))
}
