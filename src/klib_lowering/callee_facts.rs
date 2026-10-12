//! The provider records of the dependency declarations a checked call selected, by exact KLIB
//! identity.
//!
//! The body library's producer graph resolves a serialized call to an exact module-qualified
//! identity before this table is consulted. When a checked source call also selected that identity,
//! its provider record (normalized parameters, result and parameter identities, exactly as the
//! backend handoff froze them) must agree with the serialized header. A bare signature is never a
//! key and an unrelated source selection is never an origin fallback.

use crate::fx_hash::HashMap;

use crate::libraries::{KlibBodyCallable, KlibDependencyCallable};

/// Frozen dependency declarations by their exact module-qualified KLIB identity.
#[derive(Default)]
pub struct KlibCalleeFacts<'a> {
    callables: HashMap<&'a KlibDependencyCallable, KlibCalleeFact<'a>>,
}

/// What the table holds for one signature.
#[derive(Clone, Copy)]
pub(super) enum KlibCalleeFact<'a> {
    Selected(KlibBodyCallable<'a>),
    /// Two frozen declarations claim the signature. Each identity names one declaration, so the
    /// table answers for neither rather than picking one.
    Ambiguous,
}

impl<'a> KlibCalleeFacts<'a> {
    /// Index the frozen views of a caller's selected dependency declarations.
    pub fn new(callables: impl IntoIterator<Item = KlibBodyCallable<'a>>) -> Self {
        let mut facts = Self::default();
        for callable in callables {
            facts
                .callables
                .entry(callable.identity())
                .and_modify(|fact| *fact = KlibCalleeFact::Ambiguous)
                .or_insert(KlibCalleeFact::Selected(callable));
        }
        facts
    }

    pub(super) fn get(&self, identity: &KlibDependencyCallable) -> Option<KlibCalleeFact<'a>> {
        self.callables.get(identity).copied()
    }
}
