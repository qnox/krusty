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
    pub dispatch_source_name: Option<SigNameId>,
    /// Exact source origin of the `by` keyword.
    pub by_origin: OriginId,
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
    pub dispatch_source_name: Option<Box<str>>,
    pub by_origin: OriginId,
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
