//! Alias provenance recorded by authoritative compact type resolution.
//!
//! A later inferred-result publication may need the selected alias for Kotlin metadata. It reads
//! this origin-keyed fact; it never repeats classifier lookup from the source spelling.

use crate::fir::{OriginId, ResolvedTypeAbbreviation};
use crate::fx_hash::HashMap;
use std::cell::RefCell;

#[derive(Default)]
pub(super) struct SelectedTypeFacts {
    abbreviations: RefCell<HashMap<OriginId, ResolvedTypeAbbreviation>>,
}

impl SelectedTypeFacts {
    pub(super) fn record(&self, origin: OriginId, abbreviation: ResolvedTypeAbbreviation) {
        self.abbreviations.borrow_mut().insert(origin, abbreviation);
    }

    pub(super) fn abbreviation(&self, origin: OriginId) -> Option<ResolvedTypeAbbreviation> {
        self.abbreviations.borrow().get(&origin).cloned()
    }
}
