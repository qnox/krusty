//! The return-value statuses a module publishes: one fact table per declaration kind, plus the
//! checker mode the statuses were resolved under. Kotlin metadata and the annotations decide every
//! status; this table only records them, once per declaration.

use super::{DeclarationId, PropertyId};
use crate::features::ReturnValueCheckerMode;
use crate::fx_hash::HashMap;
use crate::types::ReturnValueStatus;

#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct ReturnValueFacts {
    mode: ReturnValueCheckerMode,
    properties: HashMap<PropertyId, ReturnValueStatus>,
    /// Keyed by the constructor's declaration, or by its classifier for an implicit primary one.
    constructors: HashMap<DeclarationId, ReturnValueStatus>,
}

impl ReturnValueFacts {
    pub(super) fn is_empty(&self) -> bool {
        self.properties.is_empty() && self.constructors.is_empty()
    }

    pub(super) fn mode(&self) -> ReturnValueCheckerMode {
        self.mode
    }

    pub(super) fn publish_mode(&mut self, mode: ReturnValueCheckerMode) {
        self.mode = mode;
    }

    pub(super) fn property(&self, property: PropertyId) -> ReturnValueStatus {
        self.properties.get(&property).copied().unwrap_or_default()
    }

    pub(super) fn constructor(&self, constructor: DeclarationId) -> ReturnValueStatus {
        self.constructors
            .get(&constructor)
            .copied()
            .unwrap_or_default()
    }

    pub(super) fn publish_property(&mut self, property: PropertyId, status: ReturnValueStatus) {
        if status != ReturnValueStatus::Unspecified {
            assert!(
                self.properties.insert(property, status).is_none(),
                "a property may publish its return-value status only once"
            );
        }
    }

    pub(super) fn publish_constructor(
        &mut self,
        constructor: DeclarationId,
        status: ReturnValueStatus,
    ) {
        if status != ReturnValueStatus::Unspecified {
            assert!(
                self.constructors.insert(constructor, status).is_none(),
                "a constructor may publish its return-value status only once"
            );
        }
    }
}
