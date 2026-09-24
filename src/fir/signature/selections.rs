//! Packed deferred selections owned by the temporary signature graph.

use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeferredCallableSelection {
    pub scope: SignatureScopeId,
    pub spelling: SigNameId,
    pub lexical_classifier: Option<DeclarationId>,
    pub origin: OriginId,
    pub expected: Option<SigExprId>,
    pub type_arguments: OperandRange,
    pub trailing_lambda: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeferredMemberSelection {
    pub scope: SignatureScopeId,
    pub spelling: SigNameId,
    pub origin: OriginId,
    pub expected: Option<SigExprId>,
    pub type_arguments: OperandRange,
    pub trailing_lambda: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeferredValueSelection {
    pub scope: SignatureScopeId,
    pub spelling: SigNameId,
    pub origin: OriginId,
    pub expected: Option<SigExprId>,
}
