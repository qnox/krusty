//! Semantic roles of source-local callables carried from syntax inventory through target naming.

/// What one step of a lifted callable's lexical path denotes.
///
/// A source spelling is optional data, not the callable's identity: lambdas and generated local
/// delegated-property accessors are both unnamed but have different nesting semantics.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum LiftingCallableKind {
    Lambda,
    LocalFunction,
    LocalDelegatedPropertyAccessor,
}
