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
    callables: HashMap<&'a KlibDependencyCallable, IndexedKlibCallee<'a>>,
}

enum IndexedKlibCallee<'a> {
    Selected(Vec<KlibBodyCallable<'a>>),
    /// Two frozen declarations claim the signature. Each identity names one declaration, so the
    /// table answers for neither rather than picking one.
    Ambiguous,
}

/// What one exact identity contributes to a lowering operation.
#[derive(Clone, Copy)]
pub(super) enum KlibCalleeFact<'facts, 'callable> {
    /// One declaration selected through one or more exact call-edge singleton variants.
    Selected(&'facts [KlibBodyCallable<'callable>]),
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
                .and_modify(|fact| match fact {
                    IndexedKlibCallee::Selected(selected)
                        if selected[0].same_declaration_contract(callable) =>
                    {
                        if !selected.iter().any(|selected| {
                            selected.singleton_dispatch() == callable.singleton_dispatch()
                        }) {
                            selected.push(callable);
                        }
                    }
                    fact => *fact = IndexedKlibCallee::Ambiguous,
                })
                .or_insert_with(|| IndexedKlibCallee::Selected(vec![callable]));
        }
        facts
    }

    pub(super) fn get<'facts>(
        &'facts self,
        identity: &KlibDependencyCallable,
    ) -> Option<KlibCalleeFact<'facts, 'a>> {
        match self.callables.get(identity)? {
            IndexedKlibCallee::Selected(selected) => {
                Some(KlibCalleeFact::Selected(selected.as_slice()))
            }
            IndexedKlibCallee::Ambiguous => Some(KlibCalleeFact::Ambiguous),
        }
    }
}
