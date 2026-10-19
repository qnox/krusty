//! Constructor parameter traversal after default-argument mapping.

use crate::ir::{ExprId, IrFile};
use crate::types::Ty;

/// The parameter types a construction's arguments cross into. A checked construction records its
/// selected declaration's parameters, which decide whether a value class crosses as its carrier or
/// as its box: `Holder<T>(T)` called as `Holder(Money(7))` keeps `T`, so the argument is boxed even
/// though the node's specialized `ctor_params` say `Money`. A construction the backend synthesized
/// has no checked declaration and carries only its own `ctor_params`.
pub(super) fn parameters<'a>(
    ir: &'a IrFile,
    construction: ExprId,
    ctor_params: &'a Option<Vec<Ty>>,
) -> &'a [Ty] {
    match ir.construction_declared_params.get(&construction) {
        Some(declared) => declared,
        None => ctor_params.as_deref().unwrap_or(&[]),
    }
}

/// The parameters that have supplied arguments. Prefix parameters are never defaultable; defaults
/// index only the declaration-owned suffix.
pub(super) fn supplied_parameters<'a>(
    parameters: &'a [Ty],
    defaults: &'a [u32],
    prefix_count: u32,
) -> impl Iterator<Item = &'a Ty> {
    let prefix_count = prefix_count as usize;
    parameters
        .iter()
        .enumerate()
        .filter_map(move |(parameter, ty)| {
            (parameter < prefix_count
                || !defaults.contains(&u32::try_from(parameter - prefix_count).ok()?))
            .then_some(ty)
        })
}
