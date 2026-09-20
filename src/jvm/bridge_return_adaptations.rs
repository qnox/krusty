//! JVM return adaptations selected for common bridge declarations.
//!
//! Common IR owns the declaration bridge; the JVM value-class pass records the physical return
//! adapter here, and JVM bridge emission consumes it. Keeping this side table in the backend avoids
//! leaking a target representation plan into [`crate::ir::IrFile`].

use std::collections::HashMap;

use crate::types::TypeName;

/// How one bridge's returned value is unboxed to the carrier in its JVM descriptor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BridgeReturnUnboxing {
    pub(crate) owner: TypeName,
    pub(crate) null_preserving: bool,
}

impl BridgeReturnUnboxing {
    pub(crate) fn new(owner: TypeName, null_preserving: bool) -> Self {
        Self {
            owner,
            null_preserving,
        }
    }
}

/// Per-file physical bridge plans, keyed by stable owning-class identity and bridge ordinal.
#[derive(Default)]
pub(crate) struct BridgeReturnAdaptations {
    entries: HashMap<(TypeName, u32), BridgeReturnUnboxing>,
}

impl BridgeReturnAdaptations {
    pub(crate) fn extend(
        &mut self,
        plans: impl IntoIterator<Item = ((TypeName, u32), BridgeReturnUnboxing)>,
    ) {
        for (identity, plan) in plans {
            let previous = self.entries.insert(identity, plan);
            debug_assert!(
                previous.is_none(),
                "a bridge return is realized exactly once"
            );
        }
    }

    pub(crate) fn get(&self, owner: TypeName, bridge: u32) -> Option<&BridgeReturnUnboxing> {
        self.entries.get(&(owner, bridge))
    }
}
