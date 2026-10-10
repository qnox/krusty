//! Compact expression nodes owned by the temporary signature graph.

use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SigBinaryOperator {
    Add,
    Subtract,
    Multiply,
    Divide,
    Remainder,
    Equal,
    NotEqual,
    Less,
    LessOrEqual,
    Greater,
    GreaterOrEqual,
    BooleanAnd,
    BooleanOr,
    ReferentialEqual,
    ReferentialNotEqual,
}

/// Temporary signature expression. Every variant is `Copy` and owns no allocation; variable-length
/// operands, substitutions, names, scopes, and deferred lookups live in packed side arenas owned by
/// [`SignatureGraph`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SigExpr {
    Known(ResolvedTy),
    /// An `Int` literal keeps its value until compact semantic evaluation. This is not a published
    /// type or a retained body: it is temporary literal provenance used by Kotlin's integer-
    /// constant adaptation during overload selection, and is destroyed with the signature graph.
    IntegerLiteral(i32),
    /// An unsigned integer literal (`0u`, `2147483648u`). The payload is the full `UInt` magnitude,
    /// including values above `i32::MAX`, until a sibling primitive adapts it.
    UnsignedIntegerLiteral(u64),
    DeclarationType(DeclarationId),
    ClassifierType {
        declaration: DeclarationId,
        scope: SignatureScopeId,
    },
    Parameter {
        declaration: DeclarationId,
        index: u32,
    },
    Type {
        syntax: HeaderTypeId,
        scope: SignatureScopeId,
        origin: OriginId,
    },
    ContextualType {
        expected: SigExprId,
        syntax: HeaderTypeId,
        scope: SignatureScopeId,
        origin: OriginId,
    },
    Value(DeferredValueSelectionId),
    Call {
        target: DeferredCallableSelectionId,
        arguments: CallArgumentRange,
        /// Both semantic interpretations of parser qualification, before the root is bound.
        qualified: Option<QualifiedCallCoordinateId>,
    },
    CallableReference(DeferredCallableSelectionId),
    BoundCallableReference {
        receiver: SigExprId,
        classifier: Option<SigExprId>,
        scope: SignatureScopeId,
        root: Option<SigNameId>,
        target: DeferredCallableSelectionId,
    },
    ClassLiteral {
        receiver: SigExprId,
        classifier: Option<SigExprId>,
        scope: SignatureScopeId,
        root: Option<SigNameId>,
    },
    Member {
        receiver: SigExprId,
        lookup: DeferredMemberSelectionId,
        origin: OriginId,
    },
    MemberCall {
        receiver: SigExprId,
        target: DeferredMemberSelectionId,
        arguments: CallArgumentRange,
        origin: OriginId,
    },
    Binary {
        operator: SigBinaryOperator,
        lhs: SigExprId,
        rhs: SigExprId,
        scope: SignatureScopeId,
        origin: OriginId,
    },
    Invoke {
        callee: SigExprId,
        arguments: CallArgumentRange,
        scope: SignatureScopeId,
        origin: OriginId,
    },
    Function {
        parameters: OperandRange,
        result: SigExprId,
        context_count: u32,
        has_receiver: bool,
        suspend: bool,
    },
    ContextualParameter(DeclarationId),
    ContextualFunction {
        parameters: OperandRange,
        result: SigExprId,
        scope: SignatureScopeId,
        implicit_it: bool,
        suspend: bool,
    },
    ScopedReceiver {
        receiver: SigExprId,
        result: SigExprId,
        scope: SignatureScopeId,
    },
    /// Evaluate nested executable effects in source order before yielding the signature result.
    Sequence {
        effects: OperandRange,
        result: SigExprId,
    },
    Delegate {
        delegate: SigExprId,
        expected: Option<SigExprId>,
        scope: SignatureScopeId,
        site: SignatureDelegateSiteId,
    },
    Join {
        operands: OperandRange,
        scope: SignatureScopeId,
        origin: OriginId,
    },
    Nullable(SigExprId),
    NonNullable(SigExprId),
    /// A value read after an expression-statement call whose contract proves it non-null for the
    /// rest of the block (`assertNotNull(x)`, `requireNotNull(x)`, a same-module
    /// `returns() implies (x != null)`). The callee is only known once `call` is selected, so the
    /// proof is deferred to evaluation; without it the read keeps `value`'s type.
    ContractNarrowed {
        value: SigExprId,
        call: SigExprId,
        argument: u32,
        /// The argument is the CONDITION `value != null` rather than the value itself: the proof
        /// is a `returns() implies <argument>` effect (`assertTrue(x != null)`).
        condition: bool,
    },
    /// A lexical value after a successful non-null `as` cast that has already run. Evaluation
    /// keeps `value`'s type when it is already `target` or a subtype, and uses `target` only
    /// when the cast actually narrows.
    CastNarrowed {
        value: SigExprId,
        target: SigExprId,
        scope: SignatureScopeId,
    },
    Substitute {
        base: SigExprId,
        substitutions: SubstitutionRange,
    },
}
