//! Identity of a local delegated property.
//!
//! `OriginId` records where a FIR node came from. A property's `getValue` and `setValue` are
//! different nodes, and a synthetic node can be minted from a cause that is not the declaration,
//! so an origin cannot name the property. The checker allocates one identity from the owning
//! body's declaration when it checks the property, and every convention reference copies it.

use std::collections::HashMap;

use super::{BodyOwnerId, DelegateStorage, FirDelegateCall, ResolvedTy};

/// Checked binding shared by every read and write of one local delegated property.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LocalDelegateBinding {
    pub(crate) storage: DelegateStorage,
    pub(crate) property_ty: ResolvedTy,
    pub(crate) get_value: FirDelegateCall,
    pub(crate) set_value: Option<FirDelegateCall>,
    pub(crate) name: Box<str>,
    /// Declaration identity allocated for this property. Convention references copy it; they do
    /// not recover it from the statement origin or the property's name.
    pub(crate) declaration: LocalDelegatedPropertyId,
}

/// Declaration identity of one local delegated property.
///
/// `owner` is the body that declares it. `ordinal` counts properties of that owner in the order
/// the checker sees them, including ones declared inside its lambdas, so two properties never
/// share an identity and two files do not restart the count into each other.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct LocalDelegatedPropertyId {
    owner: BodyOwnerId,
    ordinal: u32,
}

impl LocalDelegatedPropertyId {
    pub(crate) const fn new(owner: BodyOwnerId, ordinal: u32) -> Self {
        Self { owner, ordinal }
    }

    pub const fn owner(self) -> BodyOwnerId {
        self.owner
    }

    pub const fn ordinal(self) -> u32 {
        self.ordinal
    }
}

/// Ordinals still to hand out, one sequence per owning body.
///
/// Nested checkers of one body share the session that holds this table, so a property declared
/// in a lambda does not reuse the ordinal of a property declared outside it.
#[derive(Default)]
pub(crate) struct LocalDelegatedPropertyIds {
    next: HashMap<BodyOwnerId, u32>,
}

impl LocalDelegatedPropertyIds {
    pub(crate) fn allocate(&mut self, owner: BodyOwnerId) -> LocalDelegatedPropertyId {
        let count = self.next.entry(owner).or_insert(0);
        let ordinal = *count;
        *count = ordinal
            .checked_add(1)
            .expect("too many local delegated properties");
        LocalDelegatedPropertyId::new(owner, ordinal)
    }
}
