//! The declarations each selected dependency declaration overrides, as checked FIR froze them.

use crate::fir::ExternalCallableId;
use crate::types::OverriddenDeclaration;

use super::IrFile;

/// Selected dependency declaration → the exact declarations it overrides, from the member
/// hierarchy. Keyed by the stable declaration identity rather than by a call expression, so every
/// copy of a call (an inlined body, a cloned lambda) shares it; the backend handoff freezes it into
/// that declaration's [`crate::backend::BackendCallableFact`].
pub type ExternalOverriddenDeclarations =
    std::collections::HashMap<ExternalCallableId, Box<[OverriddenDeclaration]>>;

impl IrFile {
    /// Join one checked call's overridden declarations into its selected declaration's record.
    pub(crate) fn record_external_overridden_declarations(
        &mut self,
        declaration: ExternalCallableId,
        overridden: &[OverriddenDeclaration],
    ) {
        if overridden.is_empty() {
            return;
        }
        let recorded = self
            .external_overridden_declarations
            .entry(declaration)
            .or_default();
        let additions = overridden
            .iter()
            .filter(|identity| !recorded.contains(identity))
            .cloned()
            .collect::<Vec<_>>();
        if !additions.is_empty() {
            let mut joined = recorded.to_vec();
            joined.extend(additions);
            *recorded = joined.into_boxed_slice();
        }
    }
}
