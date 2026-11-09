//! Read-only target views of the exact declaration entries common realization recorded.

use super::{ClassId, FunId, IrFile};
use crate::fx_hash::HashMap;

impl IrFile {
    /// Allocating and singleton entries, keyed by their realized function identity.
    pub fn construction_entries(&self) -> &HashMap<FunId, ClassId> {
        &self.construction_entries
    }

    /// Dispatching entries, keyed by the exact exported member declaration.
    pub fn dispatch_entries(&self) -> &HashMap<FunId, FunId> {
        &self.dispatch_entries
    }

    /// Enum initialization entries, keyed by the exact realized enum class.
    pub fn enum_initializers(&self) -> &HashMap<ClassId, FunId> {
        &self.enum_initializers
    }
}
