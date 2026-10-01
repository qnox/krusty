//! Constructor parameter traversal after default-argument mapping.

use crate::types::Ty;

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
