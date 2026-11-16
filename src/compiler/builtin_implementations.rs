//! Builtin implementation facts carried by checked common IR.
//!
//! A backend that carries a builtin value in a representation of its own (a string, a box)
//! dispatches a call of `CharSequence.length` or `Number.toLong` on such a value to the class's
//! own override. Which override that is comes from the member hierarchy's override edges, which
//! the frontend owns; the handoff freezes the answer into each member's
//! [`crate::backend::BackendCallableFact`] so no backend reads the hierarchy itself.

use crate::backend::CheckedBackendCallables;

/// Record the implementation facts produced by the checker and carried by common IR.
pub(super) fn record_checked(
    callables: &mut CheckedBackendCallables,
    ir: &crate::ir::IrFile,
    represented: crate::resolve::RepresentedOverrides<'_>,
) -> Result<(), crate::fir::ExternalCallableId> {
    // The checker's facts carry no target, so each is completed with the overrides of the
    // `represented` classes first. The checked KLIB graph froze its members' facts with the same
    // overrides (`for_checked_klib_bodies`); a member whose two complete facts disagree is
    // rejected.
    let mut implementations = crate::fx_hash::HashMap::default();
    for (&identity, facts) in &ir.external_builtin_implementations {
        implementations.insert(identity, represented.complete(identity, facts)?);
    }
    for (&identity, checked) in callables.klib_builtin_implementations() {
        match implementations.entry(identity) {
            std::collections::hash_map::Entry::Occupied(existing) if existing.get() != checked => {
                return Err(identity);
            }
            std::collections::hash_map::Entry::Occupied(_) => {}
            std::collections::hash_map::Entry::Vacant(slot) => {
                slot.insert(checked.clone());
            }
        }
    }
    callables.record_checked_builtin_implementations(&implementations)
}
