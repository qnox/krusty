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
    /// A smart-cast fact invalidated only on branches whose result may still resolve to `Nothing`.
    /// The fact reaches the continuation when any branch in this disjunctive clause is
    /// non-completing; nested nodes represent conjunctions of clauses.
    CompletionNarrowed {
        value: SigExprId,
        narrowed: SigExprId,
        completions: OperandRange,
    },
    /// A lexical value after a successful non-null `as` cast that has already run. Evaluation
    /// keeps `value`'s type when it is already `target` or a subtype, and uses `target` only
    /// when the cast actually narrows.
    CastNarrowed {
        value: SigExprId,
        target: SigExprId,
        scope: SignatureScopeId,
    },
    /// A value that flows into a declared type no generic call owns (`val c: Chan<R> = this`).
    /// Evaluation yields `value`'s type and relates it to `expected` in any active builder
    /// inference frame; the declared type itself is checked by the body check.
    ExpectedValue {
        value: SigExprId,
        expected: SigExprId,
        scope: SignatureScopeId,
    },
    Substitute {
        base: SigExprId,
        substitutions: SubstitutionRange,
    },
}

impl SignatureGraph {
    pub(in crate::fir) fn expected_value(
        &mut self,
        value: SigExprId,
        expected: SigExprId,
        scope: SignatureScopeId,
    ) -> SigExprId {
        self.add_expr(SigExpr::ExpectedValue {
            value,
            expected,
            scope,
        })
    }

    /// Whether `root` reaches one of `targets` through value-expression edges. Signature
    /// extraction binds a lambda-parameter read directly to its parameter node, so this separates
    /// a parameter declaration from a body that actually reads it.
    pub(super) fn expression_reaches_any(&self, root: SigExprId, targets: &[SigExprId]) -> bool {
        let mut pending = vec![root];
        let mut visited = crate::fx_hash::HashSet::default();
        while let Some(expression) = pending.pop() {
            if targets.contains(&expression) {
                return true;
            }
            if !visited.insert(expression) {
                continue;
            }
            match self.expr(expression) {
                Some(SigExpr::Call { arguments, .. }) => pending.extend(
                    self.call_arguments(arguments)
                        .iter()
                        .map(|argument| argument.value),
                ),
                Some(
                    SigExpr::BoundCallableReference {
                        receiver,
                        classifier,
                        ..
                    }
                    | SigExpr::ClassLiteral {
                        receiver,
                        classifier,
                        ..
                    },
                ) => {
                    pending.push(receiver);
                    pending.extend(classifier);
                }
                Some(SigExpr::Member { receiver, .. }) => pending.push(receiver),
                Some(SigExpr::MemberCall {
                    receiver,
                    arguments,
                    ..
                }) => {
                    pending.push(receiver);
                    pending.extend(
                        self.call_arguments(arguments)
                            .iter()
                            .map(|argument| argument.value),
                    );
                }
                Some(SigExpr::Binary { lhs, rhs, .. }) => {
                    pending.push(lhs);
                    pending.push(rhs);
                }
                Some(SigExpr::Invoke {
                    callee, arguments, ..
                }) => {
                    pending.push(callee);
                    pending.extend(
                        self.call_arguments(arguments)
                            .iter()
                            .map(|argument| argument.value),
                    );
                }
                Some(
                    SigExpr::Function { result, .. } | SigExpr::ContextualFunction { result, .. },
                ) => pending.push(result),
                Some(SigExpr::ScopedReceiver {
                    receiver, result, ..
                }) => {
                    pending.push(receiver);
                    pending.push(result);
                }
                Some(SigExpr::Sequence { effects, result }) => {
                    pending.extend(self.operands(effects));
                    pending.push(result);
                }
                Some(SigExpr::Delegate {
                    delegate, expected, ..
                }) => {
                    pending.push(delegate);
                    pending.extend(expected);
                }
                Some(SigExpr::Join { operands, .. }) => pending.extend(self.operands(operands)),
                Some(
                    SigExpr::Nullable(value)
                    | SigExpr::NonNullable(value)
                    | SigExpr::Substitute { base: value, .. },
                ) => pending.push(value),
                Some(SigExpr::ContractNarrowed { value, call, .. }) => {
                    pending.push(value);
                    pending.push(call);
                }
                Some(SigExpr::CompletionNarrowed {
                    value,
                    narrowed,
                    completions,
                }) => {
                    pending.push(value);
                    pending.push(narrowed);
                    pending.extend(self.operands(completions));
                }
                Some(SigExpr::CastNarrowed { value, target, .. }) => {
                    pending.push(value);
                    pending.push(target);
                }
                Some(SigExpr::ExpectedValue { value, .. }) => pending.push(value),
                Some(
                    SigExpr::Known(_)
                    | SigExpr::IntegerLiteral(_)
                    | SigExpr::UnsignedIntegerLiteral(_)
                    | SigExpr::DeclarationType(_)
                    | SigExpr::ClassifierType { .. }
                    | SigExpr::Parameter { .. }
                    | SigExpr::Type { .. }
                    | SigExpr::ContextualType { .. }
                    | SigExpr::Value(_)
                    | SigExpr::CallableReference(_)
                    | SigExpr::ContextualParameter(_),
                )
                | None => {}
            }
        }
        false
    }
}
