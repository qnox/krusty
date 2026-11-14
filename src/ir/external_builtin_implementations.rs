//! Checker-computed builtin implementations keyed by selected external declaration identity.

use crate::fir::{BuiltinImplementation, ExternalCallableId};
use crate::ir::IrFile;

impl IrFile {
    /// Carry one checked unit's builtin implementation facts into this common-IR file.
    pub(crate) fn record_external_builtin_implementations(
        &mut self,
        implementations: &crate::fx_hash::HashMap<ExternalCallableId, Box<[BuiltinImplementation]>>,
    ) -> Result<(), ExternalCallableId> {
        for (&identity, selected) in implementations {
            if let Some(existing) = self.external_builtin_implementations.get(&identity) {
                if existing != selected {
                    return Err(identity);
                }
            } else {
                self.external_builtin_implementations
                    .insert(identity, selected.clone());
            }
        }
        Ok(())
    }
}
