//! The provider records of the dependency declarations a checked call selected, by exact KLIB
//! identity.
//!
//! A KLIB body names a callee only by the public signature it was serialized with, and lowering
//! declares the callee from that signature's serialized header. When a checked call of the module
//! also selected the callee, its provider record (normalized parameters, result and parameter
//! identities, exactly as the backend handoff froze them) describes the same identity, and the
//! two views must agree. This table is those records, joined to a call by signature alone; a
//! signature it does not hold is a callee only its header describes. Nothing is reconstructed from
//! the callee's name or shape.

use std::collections::HashMap;

use crate::libraries::{KlibBodyCallable, KlibDeclarationSignature};

/// Frozen dependency declarations by their exact KLIB signature.
#[derive(Default)]
pub struct KlibCalleeFacts<'a> {
    callables: HashMap<&'a KlibDeclarationSignature, KlibCalleeFact<'a>>,
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
                .entry(callable.signature())
                .and_modify(|fact| *fact = KlibCalleeFact::Ambiguous)
                .or_insert(KlibCalleeFact::Selected(callable));
        }
        facts
    }

    pub(super) fn get(&self, signature: &KlibDeclarationSignature) -> Option<KlibCalleeFact<'a>> {
        self.callables.get(signature).copied()
    }
}
