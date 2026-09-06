//! Packed stable and body-local identities shared across FIR lifetime boundaries.

use std::collections::HashMap;

use crate::diag::Span;

pub type TextRange = Span;

macro_rules! u32_id {
    ($name:ident) => {
        #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub struct $name(u32);

        impl $name {
            pub const fn from_raw(raw: u32) -> Self {
                Self(raw)
            }

            pub const fn raw(self) -> u32 {
                self.0
            }
        }
    };
}

u32_id!(SourceFileId);
u32_id!(DeclarationId);
u32_id!(BodyOwnerId);
u32_id!(OriginId);
u32_id!(SigExprId);
u32_id!(SignatureScopeId);
u32_id!(SigNameId);
u32_id!(DeferredCallableSelectionId);
u32_id!(DeferredMemberSelectionId);
u32_id!(DeferredValueSelectionId);
u32_id!(TypeParameterId);
u32_id!(DiagnosticId);
u32_id!(LookupNameId);
u32_id!(HeaderTypeId);
u32_id!(HeaderClassifierTypeId);
u32_id!(FirExprId);
u32_id!(FirStatementId);
u32_id!(LocalValueId);
u32_id!(LocalCallableId);
u32_id!(ControlTargetId);
u32_id!(FirSamConversionId);
u32_id!(FirPlatformNarrowingId);
u32_id!(CallableId);
// Provider-owned identity of a callable outside the current source module. The provider and
// backend share the corresponding realization table; FIR carries only this opaque identity.
u32_id!(ExternalCallableId);
// Provider-owned identity of a Kotlin property outside the current source module. A target backend
// maps this semantic declaration plus the FIR read/write operation to its physical realization.
// It is deliberately distinct from ExternalCallableId: a property is not its getter, setter, or
// backing field.
u32_id!(ExternalPropertyId);
u32_id!(PropertyId);
u32_id!(DeclarationNameId);

pub(super) fn next_id(len: usize, what: &str) -> u32 {
    u32::try_from(len).unwrap_or_else(|_| panic!("too many {what}; packed FIR ids are u32"))
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum DeclarationKind {
    Function,
    Property,
    Classifier,
    EnumEntry,
    TypeAlias,
    Constructor,
    Accessor,
    Initializer,
    Script,
}

/// A declaration identity allocated while Pass-1 syntax is live. The range exists only in the
/// temporary interning key; [`StableDeclarationAnchor`] is the coordinate-free form retained by
/// finalized headers.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct DeclarationAnchor {
    pub source: SourceFileId,
    pub range: TextRange,
    pub owner: Option<DeclarationId>,
    pub kind: DeclarationKind,
    /// Distinguishes synthetic declarations sharing an owner and source range. This is an
    /// owner-local structural ordinal, never a parser arena id.
    pub sibling: u32,
}

/// Stable structural declaration coordinate. Source order is stored separately in the resolved
/// module index; no text position is needed to rebind Pass-2 parser units.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct StableDeclarationAnchor {
    pub source: SourceFileId,
    pub owner: Option<DeclarationId>,
    pub kind: DeclarationKind,
    pub sibling: u32,
}

impl From<DeclarationAnchor> for StableDeclarationAnchor {
    fn from(anchor: DeclarationAnchor) -> Self {
        Self {
            source: anchor.source,
            owner: anchor.owner,
            kind: anchor.kind,
            sibling: anchor.sibling,
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DeclarationIds {
    by_anchor: HashMap<DeclarationAnchor, DeclarationId>,
    anchors: Vec<StableDeclarationAnchor>,
    /// Same-pass coordinates parallel to `anchors`. Finalization clears this vector and the
    /// coordinate-keyed map before constructing the production Pass-2 state.
    ranges: Vec<TextRange>,
    /// Declarations anchored under each owner, in declaration-id order. Owner-scoped lookups
    /// (a class's constructors by sibling ordinal) read this instead of scanning the inventory:
    /// a scan is linear in the MODULE, and it is asked once per class, so it was quadratic.
    owned: HashMap<DeclarationId, Vec<DeclarationId>>,
}

impl DeclarationIds {
    pub fn get(&self, anchor: DeclarationAnchor) -> Option<DeclarationId> {
        self.by_anchor.get(&anchor).copied()
    }

    pub fn intern(&mut self, anchor: DeclarationAnchor) -> DeclarationId {
        if let Some(id) = self.by_anchor.get(&anchor) {
            return *id;
        }
        let id = DeclarationId::from_raw(next_id(self.anchors.len(), "declarations"));
        self.anchors.push(anchor.into());
        self.ranges.push(anchor.range);
        self.by_anchor.insert(anchor, id);
        if let Some(owner) = anchor.owner {
            // Ids are allocated in increasing order, so pushing keeps each list id-ordered.
            self.owned.entry(owner).or_default().push(id);
        }
        id
    }

    /// Every declaration whose anchor names `owner`, in declaration-id order.
    pub fn owned(&self, owner: DeclarationId) -> &[DeclarationId] {
        self.owned
            .get(&owner)
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    pub fn anchor(&self, id: DeclarationId) -> Option<DeclarationAnchor> {
        let index = id.raw() as usize;
        let anchor = *self.anchors.get(index)?;
        let range = *self.ranges.get(index)?;
        Some(DeclarationAnchor {
            source: anchor.source,
            range,
            owner: anchor.owner,
            kind: anchor.kind,
            sibling: anchor.sibling,
        })
    }

    pub fn stable_anchor(&self, id: DeclarationId) -> Option<StableDeclarationAnchor> {
        self.anchors.get(id.raw() as usize).copied()
    }

    pub fn range(&self, id: DeclarationId) -> Option<TextRange> {
        self.ranges.get(id.raw() as usize).copied()
    }

    /// Destroy every source coordinate after retained Pass-1 fragments have been checked. Stable
    /// identities, ownership, declaration kind, sibling ordinals, and source ordering remain.
    pub(crate) fn release_source_coordinates(&mut self) {
        self.by_anchor.clear();
        self.by_anchor.shrink_to_fit();
        self.ranges.clear();
        self.ranges.shrink_to_fit();
    }

    pub(crate) fn retains_source_coordinates(&self) -> bool {
        !self.by_anchor.is_empty() || !self.ranges.is_empty()
    }

    pub fn len(&self) -> usize {
        self.anchors.len()
    }

    pub fn is_empty(&self) -> bool {
        self.anchors.is_empty()
    }

    pub(crate) fn storage_payload_bytes(&self) -> usize {
        self.anchors.len() * std::mem::size_of::<StableDeclarationAnchor>()
            + self.ranges.len() * std::mem::size_of::<TextRange>()
            + self.by_anchor.len()
                * (std::mem::size_of::<DeclarationAnchor>() + std::mem::size_of::<DeclarationId>())
            + self.owned.len()
                * (std::mem::size_of::<DeclarationId>() + std::mem::size_of::<Vec<DeclarationId>>())
            + self
                .owned
                .values()
                .map(|owned| owned.len() * std::mem::size_of::<DeclarationId>())
                .sum::<usize>()
    }
}
