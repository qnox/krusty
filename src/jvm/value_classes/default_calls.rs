//! Stable provider projection for value-class adaptation of inherited default calls.

use crate::fir::{CallableId, ResolvedFunctionOverrideTarget};
use crate::ir::{Callee, IrFile};
use crate::types::{Ty, TypeName};

pub(super) fn module_provider(
    target: CallableId,
    provider: ResolvedFunctionOverrideTarget,
) -> CallableId {
    let ResolvedFunctionOverrideTarget::Module(provider) = provider else {
        panic!("external inherited-default call for {target:?} survived JVM realization");
    };
    provider
}

/// The class declaring the default provider of an inherited-default call.
pub(super) fn provider_owner(ir: &IrFile, callee: &Callee) -> Option<TypeName> {
    let Callee::ModuleWithDefaults {
        target,
        default_provider,
        ..
    } = callee
    else {
        return None;
    };
    let provider = module_provider(*target, *default_provider);
    ir.referenced_module_callables.get(&provider)?.owner
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
