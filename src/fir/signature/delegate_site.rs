use crate::fir::{DeclarationId, OriginId, ResolvedTy, SigExprId, SigNameId};

/// Declaration-owned facts needed to select and diagnose a delegated-property convention during
/// Pass 1. Statement locals have no declaration in the stable header inventory, so they retain the
/// enclosing declaration only as their diagnostic owner; their semantic site remains explicitly
/// local and is never reconstructed from that owner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SignatureDelegateSite {
    pub diagnostic_owner: DeclarationId,
    pub kind: SignatureDelegateSiteKind,
    pub mutable: bool,
    /// Original declaration spelling used only at the diagnostic boundary. This is deliberately
    /// distinct from the resolved dispatch identity.
    pub dispatch_diagnostic_name: SignatureDelegateDispatchName,
    /// Exact source origin of the `by` keyword.
    pub by_origin: OriginId,
}

/// Packed diagnostic-only dispatch spelling retained inside [`crate::fir::SigExpr`]. Two reserved
/// values distinguish no dispatch receiver from an anonymous receiver; every other value is the
/// graph-owned source-name identity. Keeping this four bytes preserves the signature node's fixed
/// allocation-free size without collapsing the semantic roles back into type/name inference.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SignatureDelegateDispatchName(u32);

impl SignatureDelegateDispatchName {
    pub const NONE: Self = Self(u32::MAX);
    pub const ANONYMOUS: Self = Self(u32::MAX - 1);

    pub fn source(name: SigNameId) -> Self {
        assert!(
            name.raw() < Self::ANONYMOUS.0,
            "signature name identity overlaps a reserved delegate diagnostic role"
        );
        Self(name.raw())
    }

    pub const fn is_none(self) -> bool {
        self.0 == Self::NONE.0
    }

    pub const fn is_anonymous(self) -> bool {
        self.0 == Self::ANONYMOUS.0
    }

    pub fn source_name(self) -> Option<SigNameId> {
        (!self.is_none() && !self.is_anonymous()).then_some(SigNameId::from_raw(self.0))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SignatureDelegateSiteKind {
    TopLevel {
        extension: bool,
    },
    /// Includes members of statement-local and anonymous classifiers. `dispatch` is the stable
    /// compact expression for the classifier self type, including captured/generic arguments.
    Member {
        dispatch: SigExprId,
        extension: bool,
    },
    StatementLocal,
}

/// A delegate site after its compact dispatch expression has been evaluated. The signature bridge
/// consumes this value directly; it never reconstructs a local-class self type from a classifier
/// spelling or an incomplete semantic table entry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedSignatureDelegateSite {
    pub diagnostic_owner: DeclarationId,
    pub kind: ResolvedSignatureDelegateSiteKind,
    pub mutable: bool,
    pub dispatch_diagnostic_name: Option<ResolvedDelegateDispatchName>,
    pub by_origin: OriginId,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResolvedDelegateDispatchName {
    Source(Box<str>),
    Anonymous,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResolvedSignatureDelegateSiteKind {
    TopLevel {
        extension: bool,
    },
    Member {
        dispatch: ResolvedTy,
        extension: bool,
    },
    StatementLocal,
}
