//! The local properties a function body declares in source, by flow identity.

use crate::fx_hash::{HashMap, HashSet};
use crate::types::Ty;

/// Every local-property declaration (plain, `lateinit`, delegated, destructured, or a loop
/// variable) records its flow identity here once, with the mutability its source declares. A
/// narrowing shadow keeps its identity, and parameters are never recorded.
#[derive(Default)]
pub(super) struct LocalProperties {
    declared: HashSet<u32>,
    source_var: HashSet<u32>,
    /// The declared type of a `val` whose non-null initializer already narrows its binding
    /// (`val x: Any? = Q` reads as `Any`).
    initializer_narrowed: HashMap<u32, Ty>,
}

impl LocalProperties {
    pub(super) fn record(&mut self, identity: u32, source_var: bool) {
        self.declared.insert(identity);
        if source_var {
            self.source_var.insert(identity);
        }
    }

    /// Record that `val identity: declared` reads as its non-null initializer's narrowing.
    pub(super) fn record_initializer_narrowing(&mut self, identity: u32, declared: Ty) {
        self.initializer_narrowed.insert(identity, declared);
    }

    /// The source-declared type of a `val` its initializer narrowed, which its binding no longer
    /// shows.
    pub(super) fn declared_before_initializer(&self, identity: u32) -> Option<Ty> {
        self.initializer_narrowed.get(&identity).copied()
    }

    pub(super) fn contains(&self, identity: &u32) -> bool {
        self.declared.contains(identity)
    }

    /// Declared `var` in source. A loop variable the counted-loop lowering mutates is not.
    pub(super) fn is_source_var(&self, identity: u32) -> bool {
        self.source_var.contains(&identity)
    }
}
