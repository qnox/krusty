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

/// The value/member interpretation of a syntactically qualified call.
///
/// The parallel callable selection retains the complete spelling for the package/classifier
/// interpretation. This coordinate retains the parser's actual receiver tree and first segment so
/// evaluation can bind that root once at scope-tower priority. If it is a value, `member` owns the
/// call and a later segment failure cannot reinterpret the root as a namespace.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct QualifiedCallCoordinate {
    pub receiver: SigExprId,
    pub root: SigNameId,
    pub first_selector: SigNameId,
    pub member: DeferredMemberSelectionId,
    pub origin: OriginId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeferredValueSelection {
    pub scope: SignatureScopeId,
    pub spelling: SigNameId,
    pub origin: OriginId,
    pub expected: Option<SigExprId>,
}
