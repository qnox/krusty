//! The local properties a function body declares in source, by flow identity.

use crate::fx_hash::HashSet;

/// Every local-property declaration (plain, `lateinit`, delegated, destructured, or a loop
/// variable) records its flow identity here once, with the mutability its source declares. A
/// narrowing shadow keeps its identity, and parameters are never recorded.
#[derive(Default)]
pub(super) struct LocalProperties {
    declared: HashSet<u32>,
    source_var: HashSet<u32>,
}

impl LocalProperties {
    pub(super) fn record(&mut self, identity: u32, source_var: bool) {
        self.declared.insert(identity);
        if source_var {
            self.source_var.insert(identity);
        }
    }

    pub(super) fn contains(&self, identity: &u32) -> bool {
        self.declared.contains(identity)
    }

    /// Declared `var` in source. A loop variable the counted-loop lowering mutates is not.
    pub(super) fn is_source_var(&self, identity: u32) -> bool {
        self.source_var.contains(&identity)
    }
}
