use std::collections::HashMap;

use crate::name_tree::FxHashMap;
use crate::types::{Ty, TypeName, Visibility};

use super::body::{DefaultArgumentStore, InlineBodyStore, ResolvedCallableHeader};
use super::header::{
    next_id, CallableId, DeclarationFlags, DeclarationId, DeclarationIds, DeclarationKind,
    DeclarationNameId, DeclarationStub, DeferredCallableSelectionId, DeferredMemberSelectionId,
    DeferredValueSelectionId, DiagnosticId, ExternalCallableId, HeaderDeclaration,
    HeaderDeclarationKind, HeaderScopeArena, HeaderSyntaxArena, HeaderTypeId, LookupNames,
    OriginId, PropertyId, SigExprId, SigNameId, SignatureScopeId, SourceFileId, SourceMap,
    StableDeclarationAnchor, TypeParameterId,
};

/// A half-open slice in the signature graph's shared operand arena.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct OperandRange {
    start: u32,
    len: u32,
}

/// A half-open slice in the signature graph's packed call-argument arena.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CallArgumentRange {
    start: u32,
    len: u32,
}

/// Source call facts needed by ordinary argument mapping and overload selection. The spelling is
/// temporary lookup input interned in the signature graph; no parser expression id survives.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SigCallArgument {
    pub value: SigExprId,
    pub name: Option<SigNameId>,
    pub spread: bool,
    /// The folded value when this argument is an INTEGER LITERAL. Kotlin lets an integer literal
    /// adopt any integer type its expected parameter asks for (`byteArrayOf(10, 20)` passes `Byte`),
    /// and overload resolution needs the literal-ness, not just `Int`, to see that. Losing it here
    /// made every such call unresolvable in an inferred signature.
    pub integer_literal: Option<i32>,
    /// Whether this argument is a LAMBDA LITERAL. Overload resolution applies lambda-specific rules
    /// to one — most visibly, a lambda always conforms to an expected `… -> Unit` regardless of what
    /// its last expression evaluates to (`value.also { sb.append(x) }`).
    pub lambda: bool,
}

/// A call argument after its compact expression has been evaluated. Names borrow the temporary
/// graph and disappear with it after signature finalization.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResolvedSigCallArgument<'a> {
    pub ty: ResolvedTy,
    pub name: Option<&'a str>,
    pub spread: bool,
    pub integer_literal: Option<i32>,
    pub lambda: bool,
    /// This provisional argument is itself a call whose generic result can be rebound by the
    /// enclosing callable's selected parameter. It is set only while probing the compact graph;
    /// materialization immediately re-evaluates the call with that expectation.
    pub contextual_call: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SigCallArgumentProbe<'a> {
    Typed(ResolvedSigCallArgument<'a>),
    PostponedLambda {
        parameter_count: u32,
        implicit_it: bool,
        name: Option<&'a str>,
        spread: bool,
    },
    PostponedCallableReference {
        name: Option<&'a str>,
        spread: bool,
    },
}

/// A half-open slice in the signature graph's shared substitution arena.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SubstitutionRange {
    start: u32,
    len: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SigSubstitution {
    pub parameter: TypeParameterId,
    pub value: SigExprId,
}

/// A stable declaration context for reconstructing the normal scope tower while the temporary graph
/// is solved. It intentionally contains no copied imports, source spelling, body text, or AST id.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SignatureScope {
    pub owner: DeclarationId,
    pub source: SourceFileId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeferredCallableSelection {
    pub scope: SignatureScopeId,
    pub spelling: SigNameId,
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
    /// Evaluate compact side effects in source order, then yield `result`. This is used only when
    /// an expression that determines a published signature contains nested executable syntax whose
    /// constraints affect that signature (for example a selected anonymous-object method).
    Sequence {
        effects: OperandRange,
        result: SigExprId,
    },
    Delegate {
        declaration: DeclarationId,
        delegate: SigExprId,
        scope: SignatureScopeId,
        origin: OriginId,
        local: bool,
    },
    Join {
        operands: OperandRange,
        scope: SignatureScopeId,
        origin: OriginId,
    },
    Nullable(SigExprId),
    NonNullable(SigExprId),
    Substitute {
        base: SigExprId,
        substitutions: SubstitutionRange,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InferredSignatureKind {
    ExpressionFunction,
    PropertyInitializer,
    /// The property's public type is explicit, but its explicit backing field has an inferred
    /// storage type. This constraint is solved independently and never replaces the property's
    /// published signature.
    BackingFieldInitializer,
    ExpressionGetter,
    DelegatedProperty,
    ExtensionExpression,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SignatureConstraint {
    pub declaration: DeclarationId,
    pub result: SigExprId,
    pub kind: InferredSignatureKind,
    pub origin: OriginId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LocalSignatureEffect {
    pub result: SigExprId,
    /// An expression-bodied local callable without an explicit result type gets its call result
    /// from this compact expression. Explicit and block-body callables retain the resolver's type;
    /// their expression is evaluated only for constraints.
    pub determines_result: bool,
}

/// Compact explicit type expressions captured while a body-local declaration's lexical aliases
/// are in scope. The expressions are resolved during Pass 1 and the whole record is destroyed with
/// the signature graph; only the resulting stable declaration signature is published.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExplicitSignatureTypes {
    pub parameters: OperandRange,
    pub result: Option<SigExprId>,
    pub receiver: Option<SigExprId>,
    /// An explicitly declared backing-field type is a property-header fact, distinct from the
    /// property's public result type. Body-local declarations keep it as a compact expression so
    /// lexical aliases can be resolved before the signature graph is destroyed.
    pub storage: Option<SigExprId>,
}

/// Classifier parent types captured while a body-local alias scope is active. Like explicit member
/// types, these expressions are temporary Pass-1 constraints; only their resolved semantic types
/// may be published into the stable classifier header.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExplicitClassifierParents {
    pub superclass: Option<SigExprId>,
    pub supertypes: OperandRange,
}

/// All temporary state for signature inference. Dropping this value drops the entire constraint
/// graph in bulk before checked FIR streaming begins.
#[derive(Default)]
pub struct SignatureGraph {
    nodes: Vec<SigExpr>,
    operands: Vec<SigExprId>,
    call_arguments: Vec<SigCallArgument>,
    substitutions: Vec<SigSubstitution>,
    names: Vec<Box<str>>,
    name_ids: HashMap<Box<str>, SigNameId>,
    scopes: Vec<SignatureScope>,
    callable_selections: Vec<DeferredCallableSelection>,
    member_selections: Vec<DeferredMemberSelection>,
    value_selections: Vec<DeferredValueSelection>,
    type_syntax: HeaderSyntaxArena,
    type_names: LookupNames,
    constraints: Vec<SignatureConstraint>,
    constraint_by_declaration: HashMap<DeclarationId, usize>,
    local_effects: HashMap<DeclarationId, LocalSignatureEffect>,
    explicit_signature_types: HashMap<DeclarationId, ExplicitSignatureTypes>,
    explicit_classifier_parents: HashMap<DeclarationId, ExplicitClassifierParents>,
}

impl SignatureGraph {
    pub fn add_type_syntax(&mut self, ty: &crate::ast::TypeRef) -> HeaderTypeId {
        self.type_syntax.add_type(ty, &mut self.type_names)
    }

    pub fn transient_type_ref(&self, id: HeaderTypeId) -> Option<crate::ast::TypeRef> {
        self.type_syntax.transient_type_ref(id, &self.type_names)
    }

    pub fn add_expr(&mut self, expression: SigExpr) -> SigExprId {
        let id = SigExprId::from_raw(next_id(self.nodes.len(), "signature expressions"));
        self.nodes.push(expression);
        id
    }

    pub fn expr(&self, id: SigExprId) -> Option<SigExpr> {
        self.nodes.get(id.raw() as usize).copied()
    }

    pub fn add_operands(&mut self, operands: impl IntoIterator<Item = SigExprId>) -> OperandRange {
        let start = next_id(self.operands.len(), "signature operands");
        self.operands.extend(operands);
        let end = next_id(self.operands.len(), "signature operands");
        OperandRange {
            start,
            len: end - start,
        }
    }

    pub fn operands(&self, range: OperandRange) -> &[SigExprId] {
        let start = range.start as usize;
        let end = start + range.len as usize;
        &self.operands[start..end]
    }

    pub fn add_call_arguments(
        &mut self,
        arguments: impl IntoIterator<Item = SigCallArgument>,
    ) -> CallArgumentRange {
        let start = next_id(self.call_arguments.len(), "signature call arguments");
        self.call_arguments.extend(arguments);
        let end = next_id(self.call_arguments.len(), "signature call arguments");
        CallArgumentRange {
            start,
            len: end - start,
        }
    }

    pub fn call_arguments(&self, range: CallArgumentRange) -> &[SigCallArgument] {
        let start = range.start as usize;
        let end = start + range.len as usize;
        &self.call_arguments[start..end]
    }

    pub fn add_substitutions(
        &mut self,
        substitutions: impl IntoIterator<Item = SigSubstitution>,
    ) -> SubstitutionRange {
        let start = next_id(self.substitutions.len(), "signature substitutions");
        self.substitutions.extend(substitutions);
        let end = next_id(self.substitutions.len(), "signature substitutions");
        SubstitutionRange {
            start,
            len: end - start,
        }
    }

    pub fn substitutions(&self, range: SubstitutionRange) -> &[SigSubstitution] {
        let start = range.start as usize;
        let end = start + range.len as usize;
        &self.substitutions[start..end]
    }

    pub fn intern_name(&mut self, spelling: &str) -> SigNameId {
        if let Some(id) = self.name_ids.get(spelling) {
            return *id;
        }
        let id = SigNameId::from_raw(next_id(self.names.len(), "signature names"));
        let spelling: Box<str> = spelling.into();
        self.names.push(spelling.clone());
        self.name_ids.insert(spelling, id);
        id
    }

    pub fn name(&self, id: SigNameId) -> Option<&str> {
        self.names.get(id.raw() as usize).map(AsRef::as_ref)
    }

    pub fn add_scope(&mut self, scope: SignatureScope) -> SignatureScopeId {
        let id = SignatureScopeId::from_raw(next_id(self.scopes.len(), "signature scopes"));
        self.scopes.push(scope);
        id
    }

    pub fn scope(&self, id: SignatureScopeId) -> Option<SignatureScope> {
        self.scopes.get(id.raw() as usize).copied()
    }

    pub fn add_callable_selection(
        &mut self,
        selection: DeferredCallableSelection,
    ) -> DeferredCallableSelectionId {
        let id = DeferredCallableSelectionId::from_raw(next_id(
            self.callable_selections.len(),
            "deferred callable selections",
        ));
        self.callable_selections.push(selection);
        id
    }

    pub fn callable_selection(
        &self,
        id: DeferredCallableSelectionId,
    ) -> Option<DeferredCallableSelection> {
        self.callable_selections.get(id.raw() as usize).copied()
    }

    pub fn add_member_selection(
        &mut self,
        selection: DeferredMemberSelection,
    ) -> DeferredMemberSelectionId {
        let id = DeferredMemberSelectionId::from_raw(next_id(
            self.member_selections.len(),
            "deferred member selections",
        ));
        self.member_selections.push(selection);
        id
    }

    pub fn member_selection(
        &self,
        id: DeferredMemberSelectionId,
    ) -> Option<DeferredMemberSelection> {
        self.member_selections.get(id.raw() as usize).copied()
    }

    pub fn add_value_selection(
        &mut self,
        selection: DeferredValueSelection,
    ) -> DeferredValueSelectionId {
        let id = DeferredValueSelectionId::from_raw(next_id(
            self.value_selections.len(),
            "deferred value selections",
        ));
        self.value_selections.push(selection);
        id
    }

    pub fn value_selection(&self, id: DeferredValueSelectionId) -> Option<DeferredValueSelection> {
        self.value_selections.get(id.raw() as usize).copied()
    }

    /// Attach a declaration's expected result type to the outer deferred selection that owns
    /// contextual generic inference. Both nodes live only in the temporary signature graph. This
    /// is used by an inferred explicit backing field: its initializer is checked against the
    /// property's declared public type exactly as in an ordinary typed initializer, without
    /// retaining either body syntax or a source coordinate.
    pub fn apply_result_expectation(&mut self, result: SigExprId, expected: SigExprId) -> bool {
        match self.expr(result) {
            Some(SigExpr::Value(selection)) => {
                self.value_selections[selection.raw() as usize].expected = Some(expected);
                true
            }
            Some(SigExpr::Call { target, .. })
            | Some(SigExpr::CallableReference(target))
            | Some(SigExpr::BoundCallableReference { target, .. }) => {
                self.callable_selections[target.raw() as usize].expected = Some(expected);
                true
            }
            Some(SigExpr::Member { lookup, .. })
            | Some(SigExpr::MemberCall { target: lookup, .. }) => {
                self.member_selections[lookup.raw() as usize].expected = Some(expected);
                true
            }
            Some(SigExpr::Sequence { result, .. })
            | Some(SigExpr::ScopedReceiver { result, .. }) => {
                self.apply_result_expectation(result, expected)
            }
            Some(SigExpr::Join { operands, .. }) => {
                let operands = self.operands(operands).to_vec();
                operands
                    .into_iter()
                    .all(|operand| self.apply_result_expectation(operand, expected))
            }
            _ => false,
        }
    }

    /// Add the graph root extracted for an inferred declaration stub. Explicit signatures and
    /// ordinary/default bodies cannot enter through this API because their stubs carry no inference
    /// kind.
    pub fn add_inferred_constraint(
        &mut self,
        stub: &DeclarationStub,
        result: SigExprId,
        origin: OriginId,
    ) {
        let kind = stub
            .signature_inference
            .expect("only an inferred declaration may own a signature constraint");
        self.add_constraint(stub.id, result, kind, origin);
    }

    fn add_constraint(
        &mut self,
        declaration: DeclarationId,
        result: SigExprId,
        kind: InferredSignatureKind,
        origin: OriginId,
    ) {
        assert!(self.expr(result).is_some(), "signature root must exist");
        assert!(
            !self.constraint_by_declaration.contains_key(&declaration),
            "a declaration may have only one inferred-signature constraint"
        );
        self.constraint_by_declaration
            .insert(declaration, self.constraints.len());
        self.constraints.push(SignatureConstraint {
            declaration,
            result,
            kind,
            origin,
        });
    }

    pub fn constraints(&self) -> &[SignatureConstraint] {
        &self.constraints
    }

    pub fn constraint(&self, declaration: DeclarationId) -> Option<SignatureConstraint> {
        self.constraint_by_declaration
            .get(&declaration)
            .and_then(|index| self.constraints.get(*index))
            .copied()
    }

    pub fn add_local_effect(&mut self, declaration: DeclarationId, effect: LocalSignatureEffect) {
        assert!(
            self.expr(effect.result).is_some(),
            "local signature effect must exist"
        );
        let previous = self.local_effects.insert(declaration, effect);
        assert!(
            previous.is_none(),
            "a local declaration may have only one effect"
        );
    }

    pub fn local_effect(&self, declaration: DeclarationId) -> Option<LocalSignatureEffect> {
        self.local_effects.get(&declaration).copied()
    }

    pub fn add_explicit_signature_types(
        &mut self,
        declaration: DeclarationId,
        parameters: impl IntoIterator<Item = SigExprId>,
        result: Option<SigExprId>,
        receiver: Option<SigExprId>,
        storage: Option<SigExprId>,
    ) {
        let parameters = self.add_operands(parameters);
        self.explicit_signature_types
            .entry(declaration)
            .or_insert(ExplicitSignatureTypes {
                parameters,
                result,
                receiver,
                storage,
            });
    }

    pub fn explicit_signature_types(
        &self,
        declaration: DeclarationId,
    ) -> Option<ExplicitSignatureTypes> {
        self.explicit_signature_types.get(&declaration).copied()
    }

    pub fn add_explicit_classifier_parents(
        &mut self,
        declaration: DeclarationId,
        superclass: Option<SigExprId>,
        supertypes: impl IntoIterator<Item = SigExprId>,
    ) {
        let supertypes = self.add_operands(supertypes);
        let previous = self.explicit_classifier_parents.insert(
            declaration,
            ExplicitClassifierParents {
                superclass,
                supertypes,
            },
        );
        assert!(
            previous.is_none(),
            "a local classifier may publish its compact parent types only once"
        );
    }

    pub fn explicit_classifier_parents(
        &self,
    ) -> impl Iterator<Item = (DeclarationId, ExplicitClassifierParents)> + '_ {
        self.explicit_classifier_parents
            .iter()
            .map(|(declaration, parents)| (*declaration, *parents))
    }

    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    pub fn operand_count(&self) -> usize {
        self.operands.len()
    }

    /// Payload owned by the temporary graph, excluding allocator bookkeeping and hash-table spare
    /// capacity. Intended for relative lifetime tests rather than process-memory accounting.
    pub fn storage_payload_bytes(&self) -> usize {
        self.nodes.len() * std::mem::size_of::<SigExpr>()
            + self.operands.len() * std::mem::size_of::<SigExprId>()
            + self.call_arguments.len() * std::mem::size_of::<SigCallArgument>()
            + self.substitutions.len() * std::mem::size_of::<SigSubstitution>()
            + self.names.iter().map(|name| name.len()).sum::<usize>()
            + self.scopes.len() * std::mem::size_of::<SignatureScope>()
            + self.callable_selections.len() * std::mem::size_of::<DeferredCallableSelection>()
            + self.member_selections.len() * std::mem::size_of::<DeferredMemberSelection>()
            + self.value_selections.len() * std::mem::size_of::<DeferredValueSelection>()
            + self.type_syntax.storage_payload_bytes()
            + self.type_names.storage_payload_bytes()
            + self.constraints.len() * std::mem::size_of::<SignatureConstraint>()
            + self.local_effects.len()
                * (std::mem::size_of::<DeclarationId>()
                    + std::mem::size_of::<LocalSignatureEffect>())
            + self.explicit_signature_types.len()
                * (std::mem::size_of::<DeclarationId>()
                    + std::mem::size_of::<ExplicitSignatureTypes>())
            + self.explicit_classifier_parents.len()
                * (std::mem::size_of::<DeclarationId>()
                    + std::mem::size_of::<ExplicitClassifierParents>())
    }
}

/// A type proven suitable for publication. The field is private: pending/error types cannot be
/// manufactured by users of the resolved module index.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResolvedTy(Ty);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UnpublishableType {
    Pending,
    Error,
}

impl ResolvedTy {
    pub fn new(ty: Ty) -> Result<Self, UnpublishableType> {
        let ty = ty.canonical_semantic();
        if ty.mentions_pending() {
            Err(UnpublishableType::Pending)
        } else if ty.mentions_error() {
            Err(UnpublishableType::Error)
        } else {
            Ok(Self(ty))
        }
    }

    pub const fn get(self) -> Ty {
        self.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedSignature {
    pub parameters: Box<[ResolvedTy]>,
    pub result: ResolvedTy,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResolvedMemberCall {
    /// A selected local inferred callable may not have a publishable result until its compact body
    /// effect is evaluated. Every other selected call carries `Some`.
    pub ty: Option<ResolvedTy>,
    pub declaration: Option<DeclarationId>,
}

/// Read-only temporary inputs exposed to the ordinary resolver while explicit headers are checked.
/// Implementations must return stable semantic identities/types; none of these lookup structures
/// may be retained in the result.
pub struct HeaderResolutionContext<'a> {
    pub syntax: &'a HeaderSyntaxArena,
    pub names: &'a LookupNames,
    pub scopes: &'a HeaderScopeArena,
    /// Stable lexical ownership used to inherit enclosing classifier type parameters. Parser arena
    /// declaration ids are intentionally unavailable.
    pub declarations: &'a DeclarationIds,
}

/// Adapter implemented beside the normal resolver/checker. The streaming layer decides only which
/// declarations are expression-inferred and which are explicit; all scope/import lookup, type
/// parameter binding, alias expansion, arity/projection checks, and diagnostics remain owned by the
/// existing frontend algorithms.
pub trait ExplicitHeaderSemantics {
    fn resolve_callable(
        &mut self,
        declaration: HeaderDeclaration,
        source: SourceFileId,
        context: &HeaderResolutionContext<'_>,
    ) -> Result<ResolvedSignature, DiagnosticId>;

    fn resolve_property(
        &mut self,
        declaration: HeaderDeclaration,
        source: SourceFileId,
        context: &HeaderResolutionContext<'_>,
    ) -> Result<ResolvedSignature, DiagnosticId>;

    fn resolve_constructor(
        &mut self,
        declaration: HeaderDeclaration,
        source: SourceFileId,
        context: &HeaderResolutionContext<'_>,
    ) -> Result<ResolvedSignature, DiagnosticId>;

    fn validate_classifier(
        &mut self,
        declaration: HeaderDeclaration,
        source: SourceFileId,
        context: &HeaderResolutionContext<'_>,
    ) -> Result<(), DiagnosticId>;

    fn validate_type_alias(
        &mut self,
        declaration: HeaderDeclaration,
        source: SourceFileId,
        context: &HeaderResolutionContext<'_>,
    ) -> Result<(), DiagnosticId>;
}

impl ResolvedSignature {
    pub fn new(
        parameters: impl IntoIterator<Item = Ty>,
        result: Ty,
    ) -> Result<Self, UnpublishableType> {
        let parameters = parameters
            .into_iter()
            .map(ResolvedTy::new)
            .collect::<Result<Vec<_>, _>>()?
            .into_boxed_slice();
        Ok(Self {
            parameters,
            result: ResolvedTy::new(result)?,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SignatureState {
    Uncomputed,
    Computing,
    Resolved(ResolvedSignature),
    Failed(DiagnosticId),
}

/// Semantic adapter for signature-constraint evaluation.
///
/// The implementation belongs beside the normal resolver/checker and must route calls, members,
/// operators, substitutions, expected types, and joins through those ordinary algorithms. The
/// solver supplies only memoisation, demand ordering, and cycle detection; it deliberately has no
/// type- or overload-inference logic of its own.
pub trait SignatureConstraintEvaluator {
    fn evaluate(
        &self,
        declaration: DeclarationId,
        result: SigExprId,
        graph: &SignatureGraph,
        demand: &mut dyn FnMut(DeclarationId) -> Result<ResolvedSignature, DiagnosticId>,
    ) -> Result<ResolvedSignature, DiagnosticId>;

    /// Create the declaration-local diagnostic emitted for an unanchored inference cycle.
    fn recursive_inference_diagnostic(&self, declaration: DeclarationId) -> DiagnosticId;

    /// Defensive diagnostic for a required declaration that has neither an explicit signature nor
    /// an inferred-signature constraint. Valid extraction should never need this path.
    fn missing_signature_diagnostic(&self, declaration: DeclarationId) -> DiagnosticId;
}

/// Normal resolver/checker operations used while walking compact signature expressions. An adapter
/// beside the production checker implements these methods by invoking the same scope tower,
/// candidate collection, overload selection, generic inference, expected-type, nullability, and
/// LUB routines used for ordinary bodies. This trait intentionally contains no fallback operation.
pub trait SignatureSemantics {
    /// Enter/leave an implicit receiver while a scoped compact expression is evaluated. This
    /// covers both receiver-function lambdas and branch-local smart casts of `this`.
    /// Implementations may keep this as a short-lived stack; it is signature-solving state and is
    /// discarded with the graph.
    fn enter_scoped_receiver(&self, _declaration: DeclarationId, _receiver: ResolvedTy) {}

    fn exit_scoped_receiver(&self, _declaration: DeclarationId) {}

    /// Enter/leave a contextually typed function literal. Every input participates in temporary
    /// signature constraints; context receivers and the extension receiver enter the lexical
    /// receiver tower in declaration order. The state is graph-local and must be discarded when
    /// signature solving ends.
    fn enter_contextual_function(
        &self,
        _declaration: DeclarationId,
        _inputs: &[ResolvedTy],
        context_receivers: &[ResolvedTy],
        receiver: Option<ResolvedTy>,
    ) {
        for context in context_receivers {
            self.enter_scoped_receiver(_declaration, *context);
        }
        if let Some(receiver) = receiver {
            self.enter_scoped_receiver(_declaration, receiver);
        }
    }

    fn exit_contextual_function(&self, declaration: DeclarationId, receiver_count: usize) {
        for _ in 0..receiver_count {
            self.exit_scoped_receiver(declaration);
        }
    }

    fn declaration_parameters(
        &self,
        declaration: DeclarationId,
    ) -> Result<Box<[ResolvedTy]>, DiagnosticId>;

    /// Approximate an inferred expression type to a denotable declaration result before it is
    /// published. Temporary projection captures may participate in call/member inference, but only
    /// the declaration's own and lexically owned type parameters may cross the signature boundary.
    fn approximate_declaration_result(
        &self,
        _declaration: DeclarationId,
        result: ResolvedTy,
    ) -> Result<ResolvedTy, DiagnosticId> {
        Ok(result)
    }

    fn classifier_type(
        &self,
        declaration: DeclarationId,
        scope: SignatureScope,
    ) -> Result<ResolvedTy, DiagnosticId>;

    fn resolve_type(
        &self,
        scope: SignatureScope,
        origin: OriginId,
        syntax: HeaderTypeId,
        graph: &SignatureGraph,
    ) -> Result<ResolvedTy, DiagnosticId>;

    fn resolve_contextual_type(
        &self,
        scope: SignatureScope,
        origin: OriginId,
        syntax: HeaderTypeId,
        expected: ResolvedTy,
        graph: &SignatureGraph,
    ) -> Result<ResolvedTy, DiagnosticId>;

    fn select_value(
        &self,
        scope: SignatureScope,
        spelling: &str,
        origin: OriginId,
        expected: Option<ResolvedTy>,
        demand: &mut dyn FnMut(DeclarationId) -> Result<ResolvedSignature, DiagnosticId>,
    ) -> Result<ResolvedTy, DiagnosticId>;

    fn select_call(
        &self,
        scope: SignatureScope,
        spelling: &str,
        origin: OriginId,
        arguments: &[ResolvedSigCallArgument<'_>],
        type_arguments: &[ResolvedTy],
        trailing_lambda: bool,
        expected: Option<ResolvedTy>,
        demand: &mut dyn FnMut(DeclarationId) -> Result<ResolvedSignature, DiagnosticId>,
    ) -> Result<ResolvedTy, DiagnosticId>;

    fn call_argument_expectations(
        &self,
        scope: SignatureScope,
        spelling: &str,
        origin: OriginId,
        arguments: &[SigCallArgumentProbe<'_>],
        type_arguments: &[ResolvedTy],
        trailing_lambda: bool,
        expected: Option<ResolvedTy>,
        demand: &mut dyn FnMut(DeclarationId) -> Result<ResolvedSignature, DiagnosticId>,
    ) -> Result<Box<[Option<ResolvedTy>]>, DiagnosticId>;

    fn select_callable_reference(
        &self,
        scope: SignatureScope,
        spelling: &str,
        origin: OriginId,
        expected: Option<ResolvedTy>,
        demand: &mut dyn FnMut(DeclarationId) -> Result<ResolvedSignature, DiagnosticId>,
    ) -> Result<ResolvedTy, DiagnosticId>;

    fn select_bound_callable_reference(
        &self,
        scope: SignatureScope,
        spelling: &str,
        origin: OriginId,
        receiver: ResolvedTy,
        unbound: bool,
        expected: Option<ResolvedTy>,
        demand: &mut dyn FnMut(DeclarationId) -> Result<ResolvedSignature, DiagnosticId>,
    ) -> Result<ResolvedTy, DiagnosticId>;

    /// Select the declaration behind the compiler-defined `::property.isInitialized` operation.
    /// The operation is valid only for a callable-reference literal that denotes a stable
    /// `lateinit` property declaration; a reflective `KProperty` type alone is deliberately
    /// insufficient. `receiver` is the already-resolved value/classifier coordinate for a
    /// qualified reference, while `None` asks the normal implicit-receiver/top-level scope tower.
    fn select_lateinit_initialized(
        &self,
        scope: SignatureScope,
        spelling: &str,
        origin: OriginId,
        receiver: Option<ResolvedTy>,
        unbound: bool,
        demand: &mut dyn FnMut(DeclarationId) -> Result<ResolvedSignature, DiagnosticId>,
    ) -> Result<ResolvedTy, DiagnosticId>;

    /// Decide whether a syntactically qualified class-literal receiver starts with a value at the
    /// current scope-tower priority. A value root wins over a same-named package or classifier.
    fn class_literal_receiver_is_value(
        &self,
        scope: SignatureScope,
        root: &str,
    ) -> Result<bool, DiagnosticId>;

    /// Decide whether the root of a qualified callable reference denotes a value. Unlike a class
    /// literal, the referenced declaration participates: a singleton classifier is a value in
    /// `Object::member`, but remains a classifier qualifier in `Object::NestedConstructor`.
    fn callable_reference_receiver_is_value(
        &self,
        scope: SignatureScope,
        root: &str,
        _target: &str,
    ) -> Result<bool, DiagnosticId> {
        self.class_literal_receiver_is_value(scope, root)
    }

    /// Type a checked class literal. `unbound` distinguishes a classifier literal
    /// (`String::class`) from a value literal (`value::class`). The platform supplies the semantic
    /// `KClass` classifier; compact signature inference never names it.
    fn class_literal_type(
        &self,
        receiver: ResolvedTy,
        unbound: bool,
    ) -> Result<ResolvedTy, DiagnosticId>;

    fn select_member(
        &self,
        scope: SignatureScope,
        spelling: &str,
        origin: OriginId,
        receiver: ResolvedTy,
        expected: Option<ResolvedTy>,
        demand: &mut dyn FnMut(DeclarationId) -> Result<ResolvedSignature, DiagnosticId>,
    ) -> Result<ResolvedTy, DiagnosticId>;

    fn select_member_call(
        &self,
        scope: SignatureScope,
        spelling: &str,
        origin: OriginId,
        receiver: ResolvedTy,
        arguments: &[ResolvedSigCallArgument<'_>],
        type_arguments: &[ResolvedTy],
        trailing_lambda: bool,
        expected: Option<ResolvedTy>,
        demand: &mut dyn FnMut(DeclarationId) -> Result<ResolvedSignature, DiagnosticId>,
    ) -> Result<ResolvedMemberCall, DiagnosticId>;

    fn member_call_argument_expectations(
        &self,
        scope: SignatureScope,
        spelling: &str,
        origin: OriginId,
        receiver: ResolvedTy,
        arguments: &[SigCallArgumentProbe<'_>],
        type_arguments: &[ResolvedTy],
        trailing_lambda: bool,
        expected: Option<ResolvedTy>,
        demand: &mut dyn FnMut(DeclarationId) -> Result<ResolvedSignature, DiagnosticId>,
    ) -> Result<Box<[Option<ResolvedTy>]>, DiagnosticId>;

    fn select_binary(
        &self,
        scope: SignatureScope,
        operator: SigBinaryOperator,
        origin: OriginId,
        lhs: ResolvedTy,
        rhs: ResolvedTy,
        demand: &mut dyn FnMut(DeclarationId) -> Result<ResolvedSignature, DiagnosticId>,
    ) -> Result<ResolvedTy, DiagnosticId>;

    fn select_invoke(
        &self,
        scope: SignatureScope,
        origin: OriginId,
        callee: ResolvedTy,
        arguments: &[ResolvedSigCallArgument<'_>],
        demand: &mut dyn FnMut(DeclarationId) -> Result<ResolvedSignature, DiagnosticId>,
    ) -> Result<ResolvedTy, DiagnosticId>;

    fn invoke_argument_expectations(
        &self,
        scope: SignatureScope,
        callee: ResolvedTy,
        arguments: &[SigCallArgumentProbe<'_>],
    ) -> Result<Box<[Option<ResolvedTy>]>, DiagnosticId>;

    fn make_function_type(
        &self,
        parameters: &[ResolvedTy],
        result: ResolvedTy,
        context_count: u32,
        has_receiver: bool,
        suspend: bool,
    ) -> Result<ResolvedTy, DiagnosticId>;

    /// Apply the expected return of a contextual function literal after its body has been checked.
    /// The production implementation delegates compatibility to the ordinary subtype relation;
    /// the graph evaluator does not own a parallel expected-type algorithm.
    fn contextual_function_result(
        &self,
        _declaration: DeclarationId,
        actual: ResolvedTy,
        expected: ResolvedTy,
    ) -> Result<ResolvedTy, DiagnosticId> {
        Ok(if expected.get() == Ty::Unit {
            expected
        } else if expected.get().mentions_ty_param() {
            actual
        } else {
            expected
        })
    }

    fn make_contextual_function_type(
        &self,
        _declaration: DeclarationId,
        parameters: &[ResolvedTy],
        result: ResolvedTy,
        context_count: u32,
        has_receiver: bool,
        suspend: bool,
    ) -> Result<ResolvedTy, DiagnosticId> {
        self.make_function_type(parameters, result, context_count, has_receiver, suspend)
    }

    fn select_delegate(
        &self,
        declaration: DeclarationId,
        scope: SignatureScope,
        origin: OriginId,
        delegate: ResolvedTy,
        local: bool,
        demand: &mut dyn FnMut(DeclarationId) -> Result<ResolvedSignature, DiagnosticId>,
    ) -> Result<ResolvedTy, DiagnosticId>;

    fn least_upper_bound(
        &self,
        scope: SignatureScope,
        origin: OriginId,
        operands: &[ResolvedTy],
    ) -> Result<ResolvedTy, DiagnosticId>;

    fn make_nullable(&self, base: ResolvedTy) -> Result<ResolvedTy, DiagnosticId>;

    fn make_non_nullable(&self, base: ResolvedTy) -> Result<ResolvedTy, DiagnosticId>;

    fn substitute(
        &self,
        base: ResolvedTy,
        substitutions: &[(TypeParameterId, ResolvedTy)],
    ) -> Result<ResolvedTy, DiagnosticId>;

    fn recursive_inference_diagnostic(&self, declaration: DeclarationId) -> DiagnosticId;

    fn missing_signature_diagnostic(&self, declaration: DeclarationId) -> DiagnosticId;
}

mod evaluate;
pub use evaluate::*;
/// Demand-driven signature solving session. It owns the complete temporary graph, ensuring the
/// graph is destroyed when `finalize` consumes the solver and before the resolved index is returned.
pub struct SignatureSolver {
    graph: SignatureGraph,
    required: Vec<DeclarationId>,
    states: HashMap<DeclarationId, SignatureState>,
    computing: Vec<DeclarationId>,
    header_failures: Vec<(DeclarationId, DiagnosticId)>,
}

impl SignatureSolver {
    pub fn new(graph: SignatureGraph, required: impl IntoIterator<Item = DeclarationId>) -> Self {
        let required = required.into_iter().collect::<Vec<_>>();
        let states = required
            .iter()
            .copied()
            .map(|declaration| (declaration, SignatureState::Uncomputed))
            .collect();
        Self {
            graph,
            required,
            states,
            computing: Vec::new(),
            header_failures: Vec::new(),
        }
    }

    /// Publish a signature resolved directly from explicit syntax. It never enters the constraint
    /// graph and cannot contain pending/error types by construction.
    pub fn publish_explicit(&mut self, declaration: DeclarationId, signature: ResolvedSignature) {
        let previous = self
            .states
            .insert(declaration, SignatureState::Resolved(signature));
        assert!(
            matches!(&previous, None | Some(SignatureState::Uncomputed)),
            "a declaration signature may be published only once"
        );
        if previous.is_none() {
            self.required.push(declaration);
        }
    }

    /// Evaluate an auxiliary constraint without publishing its result as the declaration's public
    /// signature. Dependencies still use the same demand-driven solver state, so no second lookup
    /// or inference path is introduced.
    pub fn evaluate_auxiliary_constraint(
        &mut self,
        declaration: DeclarationId,
        evaluator: &impl SignatureConstraintEvaluator,
    ) -> Result<ResolvedSignature, DiagnosticId> {
        let constraint = self
            .graph
            .constraint(declaration)
            .expect("an auxiliary signature constraint must have been extracted");
        evaluator.evaluate(
            declaration,
            constraint.result,
            &self.graph,
            &mut |dependency| {
                resolve_signature(
                    &self.graph,
                    &mut self.states,
                    &mut self.computing,
                    dependency,
                    evaluator,
                )
            },
        )
    }

    fn publish_explicit_failure(&mut self, declaration: DeclarationId, diagnostic: DiagnosticId) {
        let previous = self
            .states
            .insert(declaration, SignatureState::Failed(diagnostic));
        assert!(
            matches!(&previous, None | Some(SignatureState::Uncomputed)),
            "a declaration signature may fail only once"
        );
        if previous.is_none() {
            self.required.push(declaration);
        }
    }

    /// Resolve every explicit non-local header through the ordinary frontend adapter. Inferred
    /// functions/properties remain `Uncomputed` and are the only declarations later evaluated from
    /// `SignatureGraph`. Body-only declarations never reach the adapter.
    pub fn resolve_explicit_headers(
        &mut self,
        stubs: &[DeclarationStub],
        context: HeaderResolutionContext<'_>,
        semantics: &mut impl ExplicitHeaderSemantics,
    ) {
        for stub in stubs {
            if stub.signature_inference.is_some() {
                assert!(
                    matches!(
                        stub.kind,
                        DeclarationKind::Function | DeclarationKind::Property
                    ),
                    "only functions and properties can have expression-inferred headers"
                );
                continue;
            }
            let Some(declaration) = context.syntax.declaration(stub.id) else {
                match stub.kind {
                    DeclarationKind::Accessor
                    | DeclarationKind::EnumEntry
                    | DeclarationKind::Initializer
                    | DeclarationKind::Script => continue,
                    DeclarationKind::Function
                    | DeclarationKind::Property
                    | DeclarationKind::Classifier
                    | DeclarationKind::TypeAlias
                    | DeclarationKind::Constructor => {
                        panic!("a signature-bearing declaration must have compact header syntax")
                    }
                }
            };
            let result = match (stub.kind, declaration.kind) {
                (DeclarationKind::Function, HeaderDeclarationKind::Callable { .. }) => semantics
                    .resolve_callable(declaration, stub.source, &context)
                    .map(Some),
                (DeclarationKind::Property, HeaderDeclarationKind::Property { .. }) => semantics
                    .resolve_property(declaration, stub.source, &context)
                    .map(Some),
                (DeclarationKind::Constructor, HeaderDeclarationKind::Constructor { .. }) => {
                    semantics
                        .resolve_constructor(declaration, stub.source, &context)
                        .map(Some)
                }
                (DeclarationKind::Classifier, HeaderDeclarationKind::Classifier { .. }) => {
                    semantics
                        .validate_classifier(declaration, stub.source, &context)
                        .map(|()| None)
                }
                (DeclarationKind::TypeAlias, HeaderDeclarationKind::TypeAlias { .. }) => semantics
                    .validate_type_alias(declaration, stub.source, &context)
                    .map(|()| None),
                (
                    DeclarationKind::Accessor
                    | DeclarationKind::EnumEntry
                    | DeclarationKind::Initializer
                    | DeclarationKind::Script,
                    _,
                )
                | (
                    DeclarationKind::Function
                    | DeclarationKind::Property
                    | DeclarationKind::Classifier
                    | DeclarationKind::TypeAlias
                    | DeclarationKind::Constructor,
                    _,
                ) => panic!("stable declaration kind and compact header kind disagree"),
            };
            match result {
                Ok(Some(signature)) => self.publish_explicit(stub.id, signature),
                Ok(None) => {}
                Err(diagnostic)
                    if matches!(
                        stub.kind,
                        DeclarationKind::Function
                            | DeclarationKind::Property
                            | DeclarationKind::Constructor
                    ) =>
                {
                    crate::trace_compiler!(
                        "fir",
                        "explicit header signature failed for {:?} kind={:?}",
                        stub.id,
                        stub.kind,
                    );
                    self.publish_explicit_failure(stub.id, diagnostic);
                }
                Err(diagnostic) => {
                    crate::trace_compiler!(
                        "fir",
                        "header publication failed for {:?} kind={:?}",
                        stub.id,
                        stub.kind,
                    );
                    self.header_failures.push((stub.id, diagnostic))
                }
            }
        }
    }

    pub fn state(&self, declaration: DeclarationId) -> Option<&SignatureState> {
        self.states.get(&declaration)
    }

    pub fn graph(&self) -> &SignatureGraph {
        &self.graph
    }

    pub fn resolve(
        &mut self,
        declaration: DeclarationId,
        evaluator: &impl SignatureConstraintEvaluator,
    ) -> Result<ResolvedSignature, DiagnosticId> {
        resolve_signature(
            &self.graph,
            &mut self.states,
            &mut self.computing,
            declaration,
            evaluator,
        )
    }

    fn finish(
        mut self,
        evaluator: &impl SignatureConstraintEvaluator,
    ) -> (
        ResolvedModuleIndex,
        Vec<(DeclarationId, Option<DiagnosticId>)>,
    ) {
        for declaration in self.required.iter().copied() {
            let _ = resolve_signature(
                &self.graph,
                &mut self.states,
                &mut self.computing,
                declaration,
                evaluator,
            );
        }
        // A non-local signature can expose a body-local classifier to another ordinary Pass-2
        // body. Any inferred local member reached while solving that signature is therefore part
        // of the classifier's stable callable surface, even though an otherwise-unused local
        // member remains Pass-2-only. Preserve exactly those demand-reached, resolved constraints;
        // no unevaluated graph node crosses the boundary.
        let mut published = self
            .required
            .iter()
            .copied()
            .collect::<std::collections::HashSet<_>>();
        for constraint in self.graph.constraints() {
            if matches!(
                self.states.get(&constraint.declaration),
                Some(SignatureState::Resolved(_))
            ) && published.insert(constraint.declaration)
            {
                self.required.push(constraint.declaration);
            }
        }
        let SignatureSolver {
            graph,
            required,
            states,
            computing,
            header_failures,
        } = self;
        debug_assert!(
            computing.is_empty(),
            "signature stack must unwind before finalization"
        );
        drop(graph);
        let (index, declarations) = finalize_signatures_recovering(required, states);
        let mut declarations = header_failures
            .into_iter()
            .map(|(declaration, diagnostic)| (declaration, Some(diagnostic)))
            .chain(declarations)
            .collect::<Vec<_>>();
        declarations.sort_by_key(|(declaration, _)| declaration.raw());
        declarations.dedup_by_key(|(declaration, _)| *declaration);
        (index, declarations)
    }

    /// Force every required non-local signature and consume all temporary analysis state. The graph
    /// is dropped inside this function; only the pending-free index can cross the phase boundary.
    pub fn finalize(
        self,
        evaluator: &impl SignatureConstraintEvaluator,
    ) -> Result<ResolvedModuleIndex, SignatureFinalizationError> {
        let (index, declarations) = self.finish(evaluator);
        if declarations.is_empty() {
            Ok(index)
        } else {
            Err(SignatureFinalizationError { declarations })
        }
    }

    /// Diagnostic recovery needs the successfully finalized declarations even when sibling
    /// signatures failed. This still consumes and destroys the complete lazy graph; failed or
    /// pending types are omitted rather than represented in the returned index.
    pub(crate) fn finalize_recovering(
        self,
        evaluator: &impl SignatureConstraintEvaluator,
    ) -> (
        ResolvedModuleIndex,
        Vec<(DeclarationId, Option<DiagnosticId>)>,
    ) {
        self.finish(evaluator)
    }
}

fn resolve_signature(
    graph: &SignatureGraph,
    states: &mut HashMap<DeclarationId, SignatureState>,
    computing: &mut Vec<DeclarationId>,
    declaration: DeclarationId,
    evaluator: &impl SignatureConstraintEvaluator,
) -> Result<ResolvedSignature, DiagnosticId> {
    match states.get(&declaration) {
        Some(SignatureState::Resolved(signature)) => return Ok(signature.clone()),
        Some(SignatureState::Failed(diagnostic)) => return Err(*diagnostic),
        Some(SignatureState::Computing) => {
            crate::trace_compiler!(
                "fir",
                "signature cycle: re-entered {declaration:?} with stack {computing:?}",
            );
            let cycle_start = computing
                .iter()
                .position(|candidate| *candidate == declaration)
                .unwrap_or(computing.len());
            let cycle = computing[cycle_start..].to_vec();
            let mut requested = None;
            for member in cycle {
                let diagnostic = evaluator.recursive_inference_diagnostic(member);
                if member == declaration {
                    requested = Some(diagnostic);
                }
                states.insert(member, SignatureState::Failed(diagnostic));
            }
            let diagnostic =
                requested.unwrap_or_else(|| evaluator.recursive_inference_diagnostic(declaration));
            return Err(diagnostic);
        }
        Some(SignatureState::Uncomputed) | None => {}
    }

    let Some(constraint) = graph.constraint(declaration) else {
        crate::trace_compiler!(
            "fir",
            "signature solve: {declaration:?} has no extracted constraint",
        );
        let diagnostic = evaluator.missing_signature_diagnostic(declaration);
        states.insert(declaration, SignatureState::Failed(diagnostic));
        return Err(diagnostic);
    };
    states.insert(declaration, SignatureState::Computing);
    computing.push(declaration);
    let evaluated = evaluator.evaluate(declaration, constraint.result, graph, &mut |dependency| {
        resolve_signature(graph, states, computing, dependency, evaluator)
    });
    let popped = computing.pop();
    debug_assert_eq!(popped, Some(declaration));

    // Re-entry may already have marked this declaration as a cycle member. Never overwrite that
    // diagnostic with a value computed on top of the recursive failure.
    if let Some(SignatureState::Failed(diagnostic)) = states.get(&declaration) {
        return Err(*diagnostic);
    }
    match evaluated {
        Ok(signature) => {
            crate::trace_compiler!(
                "fir",
                "signature resolved declaration={declaration:?} parameters={:?} result={:?}",
                signature.parameters,
                signature.result,
            );
            states.insert(declaration, SignatureState::Resolved(signature.clone()));
            Ok(signature)
        }
        Err(diagnostic) => {
            crate::trace_compiler!(
                "fir",
                "signature evaluation failed for {declaration:?} (constraint result {:?} = {:?})",
                constraint.result,
                graph.expr(constraint.result),
            );
            states.insert(declaration, SignatureState::Failed(diagnostic));
            Err(diagnostic)
        }
    }
}

#[derive(Debug, Default, PartialEq)]
pub struct ResolvedModuleIndex {
    declarations: DeclarationIds,
    source_inventory: HashMap<SourceFileId, Box<[DeclarationId]>>,
    /// Package identity for each source unit. This is declaration-header context, not a path or
    /// body coordinate; common lowering copies it only for referenced cross-file declarations.
    source_packages: HashMap<SourceFileId, TypeName>,
    /// Stable declaration-stream ordinal computed while Pass-1 anchors are live. This is used for
    /// deterministic declaration/metadata ordering and Pass-2 rebinding, never for executable
    /// class initialization (which has its own semantic ordinal in the declaration header).
    source_orders: HashMap<DeclarationId, u32>,
    declaration_headers: HashMap<DeclarationId, ResolvedDeclarationHeader>,
    /// Semantic lexical owner for parser-hoisted local classifiers. Both identities are stable
    /// declaration headers; no parser node or source coordinate crosses into Pass 2.
    local_classifier_lexical_roots: HashMap<DeclarationId, DeclarationId>,
    /// Resolved declaration annotation identities retained as stable semantic header metadata.
    /// Source spellings, spans, and target-specific interpretations do not cross this boundary.
    declaration_annotations: HashMap<DeclarationId, Box<[TypeName]>>,
    /// Constant string arguments parallel to selected declaration annotations. This is compact,
    /// resolved header metadata (for example the value of `@JvmName`), not retained annotation
    /// syntax. Keeping it beside the stable declaration lets target realization consume annotation
    /// policy without reopening a parser arena or a coordinate-keyed frontend table.
    declaration_annotation_string_arguments: HashMap<(DeclarationId, u32), Box<[Box<str>]>>,
    /// Stable declarations whose resolved `@Suppress` policy permits otherwise-invisible source
    /// references while checking their bodies. Annotation occurrences remain Pass-1 syntax; only
    /// this declaration-owned semantic fact crosses into Pass 2.
    visibility_suppressions: std::collections::HashSet<DeclarationId>,
    /// Resolved source annotation policies keyed by classifier identity. These are declaration
    /// header facts; no annotation syntax or source coordinate survives finalization.
    annotation_retentions: HashMap<TypeName, crate::types::AnnotationRetention>,
    annotation_targets: HashMap<TypeName, crate::types::AnnotationTargets>,
    /// Stable resolved identity of every source classifier. Identity belongs to the declaration
    /// inventory and may be known before an ordinary body-local classifier's lexical parent header
    /// is checked in Pass 2.
    classifier_identities: HashMap<DeclarationId, TypeName>,
    classifiers: HashMap<DeclarationId, ResolvedClassifierHeader>,
    classifier_declarations: HashMap<TypeName, DeclarationId>,
    /// Complete applied semantic hierarchy for each source classifier, including the classifier
    /// itself at depth zero. Pass 1 computes this while providers are live; lowering and backends
    /// must consume this closed fact instead of reopening source/module/classpath lookup.
    classifier_hierarchies: HashMap<DeclarationId, Box<[ResolvedAppliedClassifier]>>,
    /// Exact property override edges selected while module/dependency declarations are live. These
    /// are semantic declaration headers; target erasure and bridge materialization are absent.
    property_overrides: HashMap<DeclarationId, Box<[super::ResolvedPropertyOverride]>>,
    function_overrides: HashMap<DeclarationId, Box<[super::ResolvedFunctionOverride]>>,
    type_aliases: HashMap<DeclarationId, ResolvedTypeAliasHeader>,
    signatures: HashMap<DeclarationId, ResolvedSignature>,
    /// Pass-1-resolved contract effects keyed by their stable callable declaration. The wrapper
    /// guarantees that no source `TypeRef`, pending type, or error type crosses into Pass 2.
    contracts: HashMap<DeclarationId, crate::contracts::ResolvedContract>,
    /// Source type spellings required for declaration metadata, keyed by stable identity. These are
    /// serialization sidecars for already-resolved types; they are never lookup input in Pass 2.
    declaration_spellings: HashMap<DeclarationId, crate::spelling::DeclaredSpellings>,
    /// Checked compile-time values of source properties. Pass 1 folds the bounded initializer and
    /// publishes only this provider-neutral payload; no initializer syntax or source coordinate is
    /// retained for metadata or backend consumption.
    compile_time_constants: HashMap<DeclarationId, crate::libraries::LibraryConst>,
    callables: HashMap<CallableId, ResolvedCallableHeader>,
    callable_by_declaration: HashMap<DeclarationId, CallableId>,
    /// Callable headers retained only while an invalid/locally deferred declaration has no
    /// signature. Pass-2 local publication may replace one with its finalized checked header.
    diagnostic_callables: std::collections::HashSet<CallableId>,
    classifier_type_arguments: HashMap<DeclarationId, Box<[TypeParameterId]>>,
    classifier_own_type_parameter_counts: HashMap<DeclarationId, u32>,
    pub(super) callable_parameters: HashMap<CallableId, Box<[super::ResolvedValueParameterHeader]>>,
    pub(super) callable_equality_bounds: HashMap<CallableId, ResolvedTy>,
    pub(super) callable_behaviors: HashMap<CallableId, super::ResolvedCallableBehavior>,
    pub(super) declaration_names: Vec<Box<str>>,
    declaration_name_ids: FxHashMap<Box<str>, DeclarationNameId>,
    declarations_by_name: HashMap<DeclarationNameId, Vec<DeclarationId>>,
    properties: HashMap<PropertyId, ResolvedPropertyHeader>,
    property_by_declaration: HashMap<DeclarationId, PropertyId>,
    /// Source labels parallel to a property's resolved context-parameter prefix. These are compact
    /// declaration metadata: `_` distinguishes a legacy unnamed context receiver from a named
    /// context value when common IR serializes the stable header for downstream consumers.
    property_context_parameter_names: HashMap<PropertyId, Box<[Box<str>]>>,
    pub(super) type_parameters: HashMap<(DeclarationId, u32), TypeParameterId>,
    pub(super) type_parameter_owners: Vec<(DeclarationId, u32)>,
    pub(super) type_parameter_headers: Vec<super::ResolvedTypeParameterHeader>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResolvedDeclarationHeader {
    pub kind: DeclarationKind,
    pub owner: Option<DeclarationId>,
    pub name: Option<DeclarationNameId>,
    pub visibility: Visibility,
    pub flags: DeclarationFlags,
    /// Exact position in the owning class or enum-entry `ClassInit` sequence. `None` means this
    /// declaration does not contribute an executable initialization step.
    pub initialization_order: Option<u32>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedClassifierContextParameter {
    /// `None` is the legacy unnamed `context(Type)` receiver form.
    pub name: Option<Box<str>>,
    pub ty: ResolvedTy,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedClassifierHeader {
    pub declaration: DeclarationId,
    pub classifier: TypeName,
    pub superclass: Option<ResolvedTy>,
    pub interfaces: Box<[ResolvedTy]>,
    pub interface_delegations: Box<[ResolvedInterfaceDelegation]>,
    /// Constructor-supplied implicit receivers available to every instance body, in source order.
    pub context_parameters: Box<[ResolvedClassifierContextParameter]>,
    /// Closed direct subclass identities for a sealed classifier. Pass 1 computes this from stable
    /// declarations; common-IR lowering copies it without reopening a symbol table.
    pub sealed_subclasses: Box<[TypeName]>,
}

/// One classifier in a source class's already-applied semantic hierarchy.
///
/// `applied` preserves owner type arguments (for example `I<String>`), while `classifier` is the
/// stable declaration identity used for equality and provider-independent target realization.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResolvedAppliedClassifier {
    pub classifier: TypeName,
    pub applied: ResolvedTy,
    pub depth: u32,
}

/// One source type-alias declaration after Pass-1 name/type resolution. The alias is declaration
/// metadata rather than executable syntax: its expansion is pending-free, and its spelling sidecar
/// exists only so a backend can serialize Kotlin's abbreviated type faithfully.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedTypeAliasHeader {
    pub declaration: DeclarationId,
    pub identity: TypeName,
    pub expansion: ResolvedTy,
    pub expansion_spelling: crate::spelling::Spelled,
}

/// Exact checked runtime source for the value stored in one interface-delegate field.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResolvedInterfaceDelegateSource {
    /// Declared primary-constructor parameter ordinal. Common lowering adds the already-materialized
    /// compiler prefix before reading the physical constructor value.
    ConstructorParameter(u32),
    /// Exact physical ordinal in the compiler-supplied constructor prefix. Pass 2 fixes this after
    /// anonymous-object capture discovery and carries the checked value on the construction FIR.
    SyntheticConstructorParameter(u32),
    /// The checked primary-constructor FIR contains an initializer statement at this delegation's
    /// stable ordinal. Lowering records the resulting field coordinate while consuming that body.
    ConstructorBodyInitializer,
}

/// One interface-delegation edge after semantic resolution. Besides its exact value source, the
/// header owns the forwarding declarations selected from the applied interface hierarchy. Common
/// lowering only materializes this plan; it never reopens a scope or provider graph.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedInterfaceDelegation {
    pub interface: ResolvedTy,
    pub source: ResolvedInterfaceDelegateSource,
    /// Exact forwarding declaration order selected in Pass 1. Function/property interleaving is a
    /// declaration fact and cannot be reconstructed from separate lowering loops.
    pub members: Box<[ResolvedDelegatedMember]>,
}

/// Stable identity of one already-selected delegated accessor/function invocation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResolvedDelegatedModuleTarget {
    Function(CallableId),
    PropertyGetter(PropertyId),
    PropertySetter(PropertyId),
}

/// Provider-neutral realization of an already-selected delegated call. A current-module target
/// carries its stable identity plus its declaration signature. A dependency target carries the
/// provider-owned identity that target realization already understands.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResolvedDelegatedCallTarget {
    Module {
        target: ResolvedDelegatedModuleTarget,
        owner: TypeName,
        name: Box<str>,
        parameters: Box<[ResolvedTy]>,
        result: ResolvedTy,
        interface: bool,
    },
    External(ExternalCallableId),
}

/// Complete checked call shape used by one generated delegation forwarder.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedDelegatedCall {
    pub target: ResolvedDelegatedCallTarget,
    pub receiver: ResolvedTy,
    pub parameters: Box<[ResolvedTy]>,
    pub result: ResolvedTy,
    pub declared_result: Option<ResolvedTy>,
    pub suspend: bool,
    /// Semantic parameter occupied by a member-extension receiver. It is already part of
    /// `parameters`; the target backend only needs to distinguish it from a value parameter.
    pub extension_receiver_parameter: Option<u32>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedDelegatedFunction {
    pub name: Box<str>,
    /// Function-owned generic parameters of the selected interface declaration. A generated
    /// forwarder is itself a generic declaration, so these identities must travel with the checked
    /// delegation plan; common lowering and metadata must not rediscover them from parameter types.
    pub type_parameters: Box<[ResolvedDelegatedTypeParameter]>,
    /// Exact interface declaration implemented by the generated forwarder, with its unapplied
    /// semantic signature. A representation backend can derive any required dispatch bridge from
    /// this edge without repeating member lookup.
    pub overridden: ResolvedDelegatedFunctionDeclaration,
    pub call: ResolvedDelegatedCall,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedDelegatedTypeParameter {
    pub name: Box<str>,
    pub semantic_name: Box<str>,
    pub bounds: Box<[ResolvedTy]>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedDelegatedFunctionDeclaration {
    pub target: super::ResolvedFunctionOverrideTarget,
    pub owner: TypeName,
    pub parameters: Box<[ResolvedTy]>,
    pub result: ResolvedTy,
    pub interface: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResolvedDelegatedMember {
    Function(ResolvedDelegatedFunction),
    Property(ResolvedDelegatedProperty),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedDelegatedProperty {
    pub name: Box<str>,
    pub ty: ResolvedTy,
    pub context_parameters: Box<[ResolvedDelegatedContextParameter]>,
    pub getter: ResolvedDelegatedCall,
    pub setter: Option<ResolvedDelegatedCall>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedDelegatedContextParameter {
    pub name: Box<str>,
    pub ty: ResolvedTy,
}

impl ResolvedDelegatedCall {
    fn storage_payload_bytes(&self) -> usize {
        self.parameters.len() * std::mem::size_of::<ResolvedTy>()
            + match &self.target {
                ResolvedDelegatedCallTarget::Module {
                    name, parameters, ..
                } => name.len() + parameters.len() * std::mem::size_of::<ResolvedTy>(),
                ResolvedDelegatedCallTarget::External(_) => 0,
            }
    }
}

impl ResolvedInterfaceDelegation {
    fn storage_payload_bytes(&self) -> usize {
        self.members.len() * std::mem::size_of::<ResolvedDelegatedMember>()
            + self
                .members
                .iter()
                .map(|member| match member {
                    ResolvedDelegatedMember::Function(function) => {
                        function.name.len()
                            + function.type_parameters.len()
                                * std::mem::size_of::<ResolvedDelegatedTypeParameter>()
                            + function
                                .type_parameters
                                .iter()
                                .map(|parameter| {
                                    parameter.name.len()
                                        + parameter.semantic_name.len()
                                        + parameter.bounds.len() * std::mem::size_of::<ResolvedTy>()
                                })
                                .sum::<usize>()
                            + function.overridden.parameters.len()
                                * std::mem::size_of::<ResolvedTy>()
                            + function.call.storage_payload_bytes()
                    }
                    ResolvedDelegatedMember::Property(property) => {
                        property.name.len()
                            + property.context_parameters.len()
                                * std::mem::size_of::<ResolvedDelegatedContextParameter>()
                            + property
                                .context_parameters
                                .iter()
                                .map(|parameter| parameter.name.len())
                                .sum::<usize>()
                            + property.getter.storage_payload_bytes()
                            + property
                                .setter
                                .as_ref()
                                .map(ResolvedDelegatedCall::storage_payload_bytes)
                                .unwrap_or_default()
                    }
                })
                .sum::<usize>()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResolvedPropertyHeader {
    pub id: PropertyId,
    pub declaration: DeclarationId,
    pub context_parameter_count: u32,
    pub context_value_count: u32,
    pub extension_receiver: Option<ResolvedTy>,
    /// Narrower storage type of an explicit backing field, when distinct from the public property
    /// type. This is a finalized Pass-1 fact; Pass 2 must not infer it from an initializer.
    pub storage_type: Option<ResolvedTy>,
    pub mutable: bool,
}

impl ResolvedModuleIndex {
    pub(super) fn declarations_published(&self) -> bool {
        !self.declarations.is_empty()
    }

    pub fn declaration_anchor(
        &self,
        declaration: DeclarationId,
    ) -> Option<StableDeclarationAnchor> {
        self.declarations.stable_anchor(declaration)
    }

    pub fn local_classifier_lexical_root(
        &self,
        declaration: DeclarationId,
    ) -> Option<DeclarationId> {
        self.local_classifier_lexical_roots
            .get(&declaration)
            .copied()
    }

    /// Same-pass source coordinate used only while checking retained inline/default fragments.
    /// Production finalization destroys this sidecar before Pass 2 starts.
    pub(crate) fn declaration_range(
        &self,
        declaration: DeclarationId,
    ) -> Option<crate::diag::Span> {
        self.declarations.range(declaration)
    }

    pub(crate) fn release_source_coordinates(&mut self) {
        self.declarations.release_source_coordinates();
    }

    pub fn retains_source_coordinates(&self) -> bool {
        self.declarations.retains_source_coordinates()
    }

    /// Same-parse test adapter for a generated child of a body-local classifier. Production Pass 2
    /// carries capture storage directly in checked FIR and never grows the finalized module index.
    #[cfg(test)]
    pub(crate) fn intern_checked_local_declaration(
        &mut self,
        anchor: crate::fir::DeclarationAnchor,
        header: ResolvedDeclarationHeader,
        emission_name: &str,
    ) -> DeclarationId {
        assert!(
            header.flags.has(super::DeclarationFlags::LOCAL_CLASS),
            "Pass 2 may intern only body-local declarations"
        );
        let declaration = self.declarations.intern(anchor);
        if !self.source_orders.contains_key(&declaration) {
            if let Some(order) = anchor
                .owner
                .and_then(|owner| self.source_orders.get(&owner).copied())
            {
                self.source_orders.insert(declaration, order);
            }
        }
        if !self.declaration_headers.contains_key(&declaration) {
            self.publish_declaration_header(declaration, header, Some(emission_name));
        }
        declaration
    }

    /// Same-parse compatibility lookup for legacy/test checker entry points. Production Pass 2
    /// binds fresh parser declarations through [`super::ActiveSourceDeclarations`] and must not
    /// use a retained Pass-1 range as a source locator.
    pub fn declaration_at(
        &self,
        source: SourceFileId,
        range: crate::diag::Span,
        kind: DeclarationKind,
    ) -> Option<DeclarationId> {
        (0..self.declaration_count())
            .filter_map(|raw| {
                let declaration = DeclarationId::from_raw(
                    u32::try_from(raw).expect("too many stable declarations for a packed id"),
                );
                self.declaration_anchor(declaration)
                    .filter(|anchor| {
                        anchor.source == source
                            && self.declaration_range(declaration) == Some(range)
                            && anchor.kind == kind
                    })
                    .map(|_| declaration)
            })
            // Header extraction can retain a parser-ancestry alias beside the repaired semantic
            // local-class identity. Only the published identity may enter checked FIR, matching
            // `ActiveSourceDeclarations::canonical_classifier_declaration` in production Pass 2.
            .max_by_key(|declaration| self.declaration_header(*declaration).is_some())
    }

    /// Find a declaration by its stable owner-local structural coordinate. This is used while
    /// converting a checker-selected source member into checked FIR; the returned declaration ID,
    /// rather than the structural ordinal, is the only identity published to later phases.
    pub fn owned_declaration(
        &self,
        owner: DeclarationId,
        kind: DeclarationKind,
        sibling: u32,
    ) -> Option<DeclarationId> {
        (0..self.declaration_count()).find_map(|raw| {
            let declaration = DeclarationId::from_raw(
                u32::try_from(raw).expect("too many stable declarations for a packed id"),
            );
            self.declaration_anchor(declaration)
                .filter(|anchor| {
                    anchor.owner == Some(owner) && anchor.kind == kind && anchor.sibling == sibling
                })
                .map(|_| declaration)
        })
    }

    pub fn declaration_count(&self) -> usize {
        self.declarations.len()
    }

    pub fn contract(
        &self,
        declaration: DeclarationId,
    ) -> Option<&crate::contracts::ResolvedContract> {
        self.contracts.get(&declaration)
    }

    pub(crate) fn publish_contract(
        &mut self,
        declaration: DeclarationId,
        contract: crate::contracts::ResolvedContract,
    ) {
        assert!(
            self.signatures.contains_key(&declaration),
            "a contract requires a published callable signature"
        );
        assert!(
            self.contracts.insert(declaration, contract).is_none(),
            "a source contract may be published only once"
        );
    }

    pub fn declaration_spellings(
        &self,
        declaration: DeclarationId,
    ) -> Option<&crate::spelling::DeclaredSpellings> {
        self.declaration_spellings.get(&declaration)
    }

    pub(crate) fn publish_declaration_spellings(
        &mut self,
        declaration: DeclarationId,
        spellings: crate::spelling::DeclaredSpellings,
    ) {
        assert!(
            self.declaration_headers.contains_key(&declaration),
            "declaration spellings require a published semantic header"
        );
        assert!(
            self.declaration_spellings
                .insert(declaration, spellings)
                .is_none(),
            "a declaration may publish source spellings only once"
        );
    }

    pub fn compile_time_constant(
        &self,
        declaration: DeclarationId,
    ) -> Option<&crate::libraries::LibraryConst> {
        self.compile_time_constants.get(&declaration)
    }

    pub(crate) fn publish_compile_time_constant(
        &mut self,
        declaration: DeclarationId,
        constant: crate::libraries::LibraryConst,
    ) {
        assert!(
            self.property_for_declaration(declaration).is_some(),
            "a compile-time constant requires a published property identity"
        );
        assert!(
            !constant.ty.mentions_pending() && !constant.ty.mentions_error(),
            "a compile-time constant must carry a finalized semantic type"
        );
        assert!(
            self.compile_time_constants
                .insert(declaration, constant)
                .is_none(),
            "a property may publish one compile-time constant"
        );
    }

    pub(crate) fn source_inventory(&self, source: SourceFileId) -> &[DeclarationId] {
        self.source_inventory
            .get(&source)
            .map(Box::as_ref)
            .unwrap_or_default()
    }

    pub fn source_package(&self, source: SourceFileId) -> Option<TypeName> {
        self.source_packages.get(&source).copied()
    }

    /// Whether the finalized source module contributes the direct package child `name` below
    /// `parent`.
    ///
    /// Pass 2 uses this while resolving qualified imports. The answer is derived solely from
    /// stable source-package identities; retaining the legacy `SymbolTable::source_packages`
    /// prefix set would duplicate the same declaration-header fact across the pass boundary.
    pub(crate) fn source_package_child_exists(&self, parent: TypeName, name: &str) -> bool {
        let Some(candidate) = crate::types::existing_type_name_child(parent, name) else {
            return false;
        };
        self.source_packages.values().copied().any(|package| {
            let mut current = Some(package);
            while let Some(namespace) = current {
                if namespace == candidate {
                    return true;
                }
                if namespace == TypeName::ROOT {
                    break;
                }
                current = namespace.parent();
            }
            false
        })
    }

    pub(super) fn publish_source_package(&mut self, source: SourceFileId, package: TypeName) {
        assert!(
            self.source_packages.insert(source, package).is_none(),
            "a source unit may publish one package identity"
        );
    }

    pub fn source_order(&self, declaration: DeclarationId) -> Option<u32> {
        let order = self.source_orders.get(&declaration).copied();
        if order.is_none() {
            crate::trace_compiler!(
                "fir",
                "missing source order for {declaration:?}: anchor={:?}, header={:?}",
                self.declaration_anchor(declaration),
                self.declaration_header(declaration),
            );
        }
        order
    }

    pub fn declaration_header(
        &self,
        declaration: DeclarationId,
    ) -> Option<ResolvedDeclarationHeader> {
        self.declaration_headers.get(&declaration).copied()
    }

    pub fn declaration_annotations(&self, declaration: DeclarationId) -> &[TypeName] {
        self.declaration_annotations
            .get(&declaration)
            .map(Box::as_ref)
            .unwrap_or_default()
    }

    pub fn declaration_annotation_string_arguments(
        &self,
        declaration: DeclarationId,
        annotation_ordinal: u32,
    ) -> &[Box<str>] {
        self.declaration_annotation_string_arguments
            .get(&(declaration, annotation_ordinal))
            .map(Box::as_ref)
            .unwrap_or_default()
    }

    pub(crate) fn declaration_suppresses_visibility(&self, declaration: DeclarationId) -> bool {
        self.visibility_suppressions.contains(&declaration)
    }

    pub(crate) fn publish_visibility_suppression(&mut self, declaration: DeclarationId) {
        assert!(
            self.declaration_headers.contains_key(&declaration),
            "visibility suppression requires a finalized declaration header"
        );
        self.visibility_suppressions.insert(declaration);
    }

    pub(crate) fn publish_annotation_policy(
        &mut self,
        classifier: TypeName,
        retention: crate::types::AnnotationRetention,
        targets: Option<crate::types::AnnotationTargets>,
    ) {
        assert!(
            self.classifier_declarations.contains_key(&classifier),
            "annotation policy requires a finalized classifier header"
        );
        assert!(
            self.annotation_retentions
                .insert(classifier, retention)
                .is_none(),
            "an annotation classifier may publish one retention policy"
        );
        if let Some(targets) = targets {
            assert!(
                self.annotation_targets
                    .insert(classifier, targets)
                    .is_none(),
                "an annotation classifier may publish one target policy"
            );
        }
    }

    pub(crate) fn annotation_retention(
        &self,
        classifier: TypeName,
    ) -> Option<crate::types::AnnotationRetention> {
        self.annotation_retentions.get(&classifier).copied()
    }

    pub(crate) fn annotation_targets(
        &self,
        classifier: TypeName,
    ) -> crate::types::AnnotationTargets {
        self.annotation_targets
            .get(&classifier)
            .copied()
            .unwrap_or(crate::types::AnnotationTargets::DEFAULT)
    }

    pub fn classifier_header(
        &self,
        declaration: DeclarationId,
    ) -> Option<&ResolvedClassifierHeader> {
        self.classifiers.get(&declaration)
    }

    /// Resolved classifier identity independent of whether its complete semantic parent header is
    /// already available. This is the only Pass-2 bridge for a deferred local classifier; callers
    /// must still require [`Self::classifier_header`] before querying hierarchy or members.
    pub fn classifier_identity(&self, declaration: DeclarationId) -> Option<TypeName> {
        self.classifier_identities.get(&declaration).copied()
    }

    pub fn classifier_declaration(&self, classifier: TypeName) -> Option<DeclarationId> {
        self.classifier_declarations.get(&classifier).copied()
    }

    pub fn classifier_hierarchy(
        &self,
        declaration: DeclarationId,
    ) -> Option<&[ResolvedAppliedClassifier]> {
        self.classifier_hierarchies
            .get(&declaration)
            .map(Box::as_ref)
    }

    pub(crate) fn publish_classifier_hierarchy(
        &mut self,
        declaration: DeclarationId,
        hierarchy: impl IntoIterator<Item = (TypeName, Ty, usize)>,
    ) {
        assert!(
            self.classifiers.contains_key(&declaration),
            "an applied hierarchy requires a published classifier header"
        );
        let hierarchy = hierarchy
            .into_iter()
            .map(|(classifier, applied, depth)| ResolvedAppliedClassifier {
                classifier,
                applied: ResolvedTy::new(applied)
                    .expect("a finalized classifier hierarchy cannot contain pending/error types"),
                depth: u32::try_from(depth).expect("classifier hierarchy depth exceeds u32"),
            })
            .collect::<Vec<_>>()
            .into_boxed_slice();
        assert!(
            hierarchy.first().is_some_and(|entry| {
                entry.depth == 0
                    && self
                        .classifiers
                        .get(&declaration)
                        .is_some_and(|header| header.classifier == entry.classifier)
            }),
            "an applied hierarchy must start with its source classifier"
        );
        assert!(
            self.classifier_hierarchies
                .insert(declaration, hierarchy)
                .is_none(),
            "a source classifier may publish its applied hierarchy only once"
        );
    }

    pub fn property_overrides(
        &self,
        classifier: DeclarationId,
    ) -> &[super::ResolvedPropertyOverride] {
        self.property_overrides
            .get(&classifier)
            .map(Box::as_ref)
            .unwrap_or_default()
    }

    pub(crate) fn has_property_override_plan(&self, classifier: DeclarationId) -> bool {
        self.property_overrides.contains_key(&classifier)
    }

    pub(crate) fn publish_property_overrides(
        &mut self,
        classifier: DeclarationId,
        overrides: impl IntoIterator<Item = super::ResolvedPropertyOverride>,
    ) {
        assert!(
            self.classifiers.contains_key(&classifier),
            "property overrides require a published classifier header"
        );
        let overrides = overrides.into_iter().collect::<Vec<_>>().into_boxed_slice();
        assert!(
            self.property_overrides
                .insert(classifier, overrides)
                .is_none(),
            "a source classifier may publish property overrides only once"
        );
    }

    pub fn function_overrides(
        &self,
        classifier: DeclarationId,
    ) -> &[super::ResolvedFunctionOverride] {
        self.function_overrides
            .get(&classifier)
            .map(Box::as_ref)
            .unwrap_or_default()
    }

    pub(crate) fn has_function_override_plan(&self, classifier: DeclarationId) -> bool {
        self.function_overrides.contains_key(&classifier)
    }

    pub(crate) fn publish_function_overrides(
        &mut self,
        classifier: DeclarationId,
        overrides: impl IntoIterator<Item = super::ResolvedFunctionOverride>,
    ) {
        assert!(
            self.classifiers.contains_key(&classifier),
            "function overrides require a published classifier header"
        );
        let overrides = overrides.into_iter().collect::<Vec<_>>().into_boxed_slice();
        assert!(
            self.function_overrides
                .insert(classifier, overrides)
                .is_none(),
            "a source classifier may publish function overrides only once"
        );
    }

    pub fn type_alias_header(
        &self,
        declaration: DeclarationId,
    ) -> Option<&ResolvedTypeAliasHeader> {
        self.type_aliases.get(&declaration)
    }

    /// Resolve a source type-alias declaration by its stable qualified identity.
    ///
    /// Pass 2 uses this semantic header instead of the legacy symbol table's spelling map. Local
    /// aliases remain lexical declarations in the active body and therefore never enter this
    /// module-level index.
    pub fn type_alias_by_identity(&self, identity: TypeName) -> Option<&ResolvedTypeAliasHeader> {
        self.type_aliases
            .values()
            .find(|header| header.identity == identity)
    }

    /// Resolve a nested type alias by stable classifier owner and source name.
    pub fn type_alias_in_classifier(
        &self,
        owner: TypeName,
        name: &str,
    ) -> Option<&ResolvedTypeAliasHeader> {
        let owner = self.classifier_declaration(owner)?;
        self.type_aliases.values().find(|alias| {
            self.declaration_header(alias.declaration)
                .is_some_and(|header| header.owner == Some(owner))
                && self.declaration_name(alias.declaration) == Some(name)
        })
    }

    /// Source parameter names owned by a finalized type alias, in declaration order.
    pub fn type_alias_formals(&self, declaration: DeclarationId) -> Vec<String> {
        (0..)
            .map_while(|ordinal| self.type_parameter(declaration, ordinal))
            .filter_map(|parameter| self.type_parameter_name(parameter).map(str::to_owned))
            .collect()
    }

    /// Type-parameter identities carried by an applied classifier type, in the same order as
    /// [`Ty::type_args`]: parameters declared by the classifier, followed by lexically captured
    /// parameters from nearest owner outward.
    pub fn classifier_type_arguments(
        &self,
        classifier: DeclarationId,
    ) -> Option<&[TypeParameterId]> {
        self.classifier_type_arguments
            .get(&classifier)
            .map(Box::as_ref)
    }

    pub fn classifier_own_type_parameter_count(&self, classifier: DeclarationId) -> Option<u32> {
        self.classifier_own_type_parameter_counts
            .get(&classifier)
            .copied()
    }

    pub(crate) fn publish_classifier_type_arguments(
        &mut self,
        classifier: DeclarationId,
        own_count: u32,
        parameters: impl IntoIterator<Item = TypeParameterId>,
    ) {
        assert!(
            self.classifiers.contains_key(&classifier),
            "classifier type arguments require a published classifier header"
        );
        let parameters = parameters.into_iter().collect::<Box<[_]>>();
        assert!(
            own_count as usize <= parameters.len(),
            "own classifier parameters must be a prefix of its applied layout"
        );
        assert!(
            self.classifier_own_type_parameter_counts
                .insert(classifier, own_count)
                .is_none()
                && self
                    .classifier_type_arguments
                    .insert(classifier, parameters)
                    .is_none(),
            "a classifier type-argument layout may be published only once"
        );
    }

    /// Bind an already-selected source constructor shape to its stable module declaration. This is
    /// identity publication, not overload selection: the resolver has already fixed primary versus
    /// secondary and the complete semantic parameter list.
    pub fn constructor_declaration(
        &self,
        classifier: TypeName,
        primary: bool,
        parameters: &[Ty],
    ) -> Option<DeclarationId> {
        let owner = self.classifier_declaration(classifier)?;
        (0..self.declaration_count()).find_map(|raw| {
            let declaration = DeclarationId::from_raw(raw as u32);
            let anchor = self.declaration_anchor(declaration)?;
            (anchor.kind == DeclarationKind::Constructor
                && anchor.owner == Some(owner)
                && (anchor.sibling == 0) == primary
                && self.signature(declaration)?.parameters.len() == parameters.len()
                // A classifier has exactly one primary constructor. Its selected call parameters
                // may already be substituted with the call site's class arguments (`Box<String>`),
                // while the stable declaration necessarily retains `Box<T>`. Primary identity is
                // therefore fixed by the owner and primary flag; comparing those two type views
                // would turn successful generic selection into a missing target. Secondary
                // constructors still need their complete shape to distinguish overloads.
                && (primary
                    || self
                        .signature(declaration)?
                        .parameters
                        .iter()
                        .zip(parameters)
                        .all(|(resolved, parameter)| resolved.get() == *parameter)))
            .then_some(declaration)
        })
    }

    /// Complete the stable identity of an already-selected source constructor from its owner and
    /// exact declaration signature. This is deliberately stricter than overload selection: no
    /// applicability or substitution is performed, and duplicate matching shapes yield no identity.
    pub fn unique_constructor_declaration(
        &self,
        classifier: TypeName,
        parameters: &[Ty],
    ) -> Option<DeclarationId> {
        let owner = self.classifier_declaration(classifier)?;
        let mut selected = None;
        for raw in 0..self.declaration_count() {
            let declaration = DeclarationId::from_raw(raw as u32);
            let Some(anchor) = self.declaration_anchor(declaration) else {
                continue;
            };
            if anchor.kind != DeclarationKind::Constructor || anchor.owner != Some(owner) {
                continue;
            }
            let Some(signature) = self.signature(declaration) else {
                continue;
            };
            if signature.parameters.len() != parameters.len()
                || !signature
                    .parameters
                    .iter()
                    .zip(parameters)
                    .all(|(resolved, parameter)| resolved.get() == *parameter)
            {
                continue;
            }
            if selected.replace(declaration).is_some() {
                return None;
            }
        }
        selected
    }

    pub fn enclosing_classifier(
        &self,
        mut declaration: DeclarationId,
    ) -> Option<&ResolvedClassifierHeader> {
        loop {
            if let Some(classifier) = self.classifier_header(declaration) {
                return Some(classifier);
            }
            declaration = self.declaration_header(declaration)?.owner?;
        }
    }

    /// Whether a declaration belongs to a classifier introduced inside an executable body. Stable
    /// anchors for those classifiers deliberately do not encode their parser statement owner, but
    /// their resolved declaration flag and every owned child edge remain sufficient to keep them
    /// out of file-level common-IR predeclaration.
    pub fn is_body_local_declaration(&self, mut declaration: DeclarationId) -> bool {
        loop {
            if self
                .declaration_header(declaration)
                .is_some_and(|header| header.flags.has(super::DeclarationFlags::LOCAL_CLASS))
            {
                return true;
            }
            let Some(owner) = self
                .declaration_anchor(declaration)
                .and_then(|anchor| anchor.owner)
            else {
                return false;
            };
            declaration = owner;
        }
    }

    pub fn publish_declaration_header(
        &mut self,
        declaration: DeclarationId,
        mut header: ResolvedDeclarationHeader,
        emission_name: Option<&str>,
    ) {
        header.name = emission_name.map(|name| self.intern_declaration_name(name));
        assert!(
            self.declaration_headers
                .insert(declaration, header)
                .is_none(),
            "a stable declaration may publish only one semantic header"
        );
        if let Some(name) = header.name {
            self.declarations_by_name
                .entry(name)
                .or_default()
                .push(declaration);
        }
    }

    pub fn publish_declaration_annotations(
        &mut self,
        declaration: DeclarationId,
        annotations: impl IntoIterator<Item = TypeName>,
    ) {
        let annotations = annotations
            .into_iter()
            .collect::<Vec<_>>()
            .into_boxed_slice();
        if annotations.is_empty() {
            return;
        }
        assert!(
            self.declaration_annotations
                .insert(declaration, annotations)
                .is_none(),
            "a stable declaration may publish its annotation identities only once"
        );
    }

    pub fn publish_declaration_annotation_string_arguments(
        &mut self,
        declaration: DeclarationId,
        annotation_ordinal: u32,
        arguments: impl IntoIterator<Item = Box<str>>,
    ) {
        let arguments = arguments.into_iter().collect::<Vec<_>>().into_boxed_slice();
        if arguments.is_empty() {
            return;
        }
        assert!(
            self.declaration_annotation_string_arguments
                .insert((declaration, annotation_ordinal), arguments)
                .is_none(),
            "a stable annotation occurrence may publish its string arguments only once"
        );
    }

    pub fn publish_classifier_header(
        &mut self,
        declaration: DeclarationId,
        classifier: TypeName,
        superclass: Option<Ty>,
        interfaces: impl IntoIterator<Item = Ty>,
        interface_delegations: impl IntoIterator<Item = ResolvedInterfaceDelegation>,
        context_parameters: impl IntoIterator<Item = (Option<Box<str>>, Ty)>,
        sealed_subclasses: impl IntoIterator<Item = TypeName>,
    ) -> Result<(), UnpublishableType> {
        let superclass = superclass.map(ResolvedTy::new).transpose()?;
        let interfaces = interfaces
            .into_iter()
            .map(ResolvedTy::new)
            .collect::<Result<Vec<_>, _>>()?
            .into_boxed_slice();
        let interface_delegations = interface_delegations
            .into_iter()
            .collect::<Vec<_>>()
            .into_boxed_slice();
        let context_parameters = context_parameters
            .into_iter()
            .map(|(name, ty)| {
                Ok(ResolvedClassifierContextParameter {
                    name,
                    ty: ResolvedTy::new(ty)?,
                })
            })
            .collect::<Result<Vec<_>, UnpublishableType>>()?
            .into_boxed_slice();
        let sealed_subclasses = sealed_subclasses
            .into_iter()
            .collect::<Vec<_>>()
            .into_boxed_slice();
        self.publish_classifier_identity(declaration, classifier);
        assert!(
            self.classifiers
                .insert(
                    declaration,
                    ResolvedClassifierHeader {
                        declaration,
                        classifier,
                        superclass,
                        interfaces,
                        interface_delegations,
                        context_parameters,
                        sealed_subclasses,
                    },
                )
                .is_none(),
            "a stable classifier may publish only one semantic header"
        );
        Ok(())
    }

    /// Publish only a classifier's stable semantic identity. Ordinary body-local parent types may
    /// depend on statement-local aliases and are completed by `publish_classifier_header` after the
    /// authoritative Pass-2 lexical check; no provisional parent edge is stored here.
    pub(crate) fn publish_classifier_identity(
        &mut self,
        declaration: DeclarationId,
        classifier: TypeName,
    ) {
        if let Some(existing) = self.classifier_identities.insert(declaration, classifier) {
            assert_eq!(
                existing, classifier,
                "a stable classifier declaration cannot change semantic identity"
            );
        }
        if let Some(existing) = self.classifier_declarations.insert(classifier, declaration) {
            assert_eq!(
                existing, declaration,
                "a stable classifier identity may have only one module declaration"
            );
        }
    }

    /// Complete the generated forwarding surface after callable/property identities have been
    /// published. Classifier identity and inheritance are available earlier, but delegation closure
    /// consumes those stable member identities and therefore has this explicit second header step.
    pub(crate) fn publish_interface_delegations(
        &mut self,
        declaration: DeclarationId,
        delegations: impl IntoIterator<Item = ResolvedInterfaceDelegation>,
    ) {
        let header = self
            .classifiers
            .get_mut(&declaration)
            .expect("interface delegation requires a published classifier header");
        assert!(
            header.interface_delegations.is_empty(),
            "interface delegation forwarding facts may be published only once"
        );
        header.interface_delegations = delegations
            .into_iter()
            .collect::<Vec<_>>()
            .into_boxed_slice();
    }

    /// Publish a local classifier's authoritative checked Pass-2 delegation plan. Pass 1 can inventory
    /// its interface edge, but local signatures and anonymous capture layout do not exist until the
    /// bounded construction unit is checked; the applied interface identity must remain stable while
    /// its runtime source and locally-specialized forwarding types become final here.
    pub(crate) fn publish_checked_local_interface_delegations(
        &mut self,
        declaration: DeclarationId,
        delegations: impl IntoIterator<Item = ResolvedInterfaceDelegation>,
    ) {
        let delegations = delegations.into_iter().collect::<Vec<_>>();
        let header = self
            .classifiers
            .get_mut(&declaration)
            .expect("interface delegation requires a published classifier header");
        if !header.interface_delegations.is_empty() {
            assert_eq!(header.interface_delegations.len(), delegations.len());
            for (stable, checked) in header.interface_delegations.iter().zip(&delegations) {
                assert_eq!(stable.interface, checked.interface);
            }
        }
        header.interface_delegations = delegations.into_boxed_slice();
    }

    pub fn publish_type_alias_header(
        &mut self,
        declaration: DeclarationId,
        identity: TypeName,
        expansion: Ty,
        expansion_spelling: crate::spelling::Spelled,
    ) -> Result<(), UnpublishableType> {
        let expansion = ResolvedTy::new(expansion)?;
        assert!(
            self.type_aliases
                .insert(
                    declaration,
                    ResolvedTypeAliasHeader {
                        declaration,
                        identity,
                        expansion,
                        expansion_spelling,
                    },
                )
                .is_none(),
            "a stable type alias may publish only one semantic header"
        );
        Ok(())
    }

    pub(super) fn publish_declarations(&mut self, declarations: DeclarationIds) {
        assert!(
            self.declarations.is_empty(),
            "stable declarations may be published only once"
        );
        self.declarations = declarations;
    }

    pub(super) fn publish_local_classifier_lexical_roots(
        &mut self,
        roots: HashMap<DeclarationId, DeclarationId>,
    ) {
        assert!(
            self.local_classifier_lexical_roots.is_empty(),
            "local-classifier lexical ownership may be published only once"
        );
        self.local_classifier_lexical_roots = roots;
    }

    pub(super) fn publish_source_inventory(
        &mut self,
        inventory: &[DeclarationId],
        declarations: &DeclarationIds,
    ) {
        assert!(
            self.source_inventory.is_empty(),
            "source declaration inventory may be published only once"
        );
        let mut grouped = HashMap::<SourceFileId, Vec<DeclarationId>>::new();
        for &declaration in inventory {
            let source = declarations
                .anchor(declaration)
                .expect("inventoried declaration must retain its stable source")
                .source;
            grouped.entry(source).or_default().push(declaration);
        }
        // `inventory` is the parser's semantic declaration stream. Preserve that order exactly:
        // ranges are diagnostic payload, and released ordinary accessor bodies may legitimately
        // fall back to their owner's span on a later bounded parse. Sorting by those spans turns a
        // getter/setter pair into a body locator and can swap their stable identities.
        for source_declarations in grouped.values() {
            for (order, declaration) in source_declarations.iter().copied().enumerate() {
                self.source_orders.insert(
                    declaration,
                    u32::try_from(order).expect("too many source declarations"),
                );
            }
        }
        // A retained inline/default body can discover compiler-generated capture storage after the
        // parser declaration stream has been inventoried. Such a field has no independent source
        // position: its exact ordering anchor is its owning declaration. Do not put it into the
        // active-source inventory (there is no Pass-2 syntax to bind); publish only that structural
        // ordering fact. Restrict this to generated declarations so a missing written declaration
        // remains an invariant failure instead of silently inheriting an owner's position.
        loop {
            let mut added = false;
            for raw in 0..declarations.len() {
                let declaration = DeclarationId::from_raw(
                    u32::try_from(raw).expect("too many stable declarations for a packed id"),
                );
                if self.source_orders.contains_key(&declaration)
                    || !self.declaration_header(declaration).is_some_and(|header| {
                        header
                            .flags
                            .has(super::DeclarationFlags::COMPILER_GENERATED)
                    })
                {
                    continue;
                }
                let Some(order) = declarations
                    .anchor(declaration)
                    .and_then(|anchor| anchor.owner)
                    .and_then(|owner| self.source_orders.get(&owner).copied())
                else {
                    continue;
                };
                self.source_orders.insert(declaration, order);
                added = true;
            }
            if !added {
                break;
            }
        }
        self.source_inventory = grouped
            .into_iter()
            .map(|(source, declarations)| (source, declarations.into_boxed_slice()))
            .collect();
    }

    pub fn signature(&self, declaration: DeclarationId) -> Option<&ResolvedSignature> {
        self.signatures.get(&declaration)
    }

    /// Publish one fully resolved non-local signature at the Pass-1 boundary. The constructor is
    /// the only way legacy migration adapters can enter this index, so `Pending` and `Error` are
    /// rejected before checked FIR or lowering can observe them.
    pub fn publish_signature(
        &mut self,
        declaration: DeclarationId,
        parameters: impl IntoIterator<Item = Ty>,
        result: Ty,
    ) -> Result<(), UnpublishableType> {
        let signature = ResolvedSignature::new(parameters, result)?;
        assert!(
            self.signatures.insert(declaration, signature).is_none(),
            "a stable declaration may publish only one resolved signature"
        );
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.signatures.len()
    }

    pub fn is_empty(&self) -> bool {
        self.declarations.is_empty()
            && self.declaration_headers.is_empty()
            && self.declaration_annotations.is_empty()
            && self.declaration_annotation_string_arguments.is_empty()
            && self.classifiers.is_empty()
            && self.signatures.is_empty()
            && self.callables.is_empty()
            && self.callable_equality_bounds.is_empty()
            && self.properties.is_empty()
    }

    pub fn callable(&self, callable: CallableId) -> Option<ResolvedCallableHeader> {
        self.callables.get(&callable).copied()
    }

    pub fn callable_for_declaration(
        &self,
        declaration: DeclarationId,
    ) -> Option<ResolvedCallableHeader> {
        self.callable_by_declaration
            .get(&declaration)
            .and_then(|callable| self.callable(*callable))
    }

    pub fn callable_name(&self, callable: CallableId) -> Option<&str> {
        let header = self.callable(callable)?;
        let super::body::ResolvedCallableName::Function(name) = header.name else {
            return None;
        };
        self.declaration_names
            .get(name.raw() as usize)
            .map(AsRef::as_ref)
    }

    pub fn declaration_name(&self, declaration: DeclarationId) -> Option<&str> {
        let name = self.declaration_header(declaration)?.name?;
        self.declaration_names
            .get(name.raw() as usize)
            .map(AsRef::as_ref)
    }

    pub(crate) fn declarations_named(&self, name: &str) -> &[DeclarationId] {
        self.declaration_name_ids
            .get(name)
            .and_then(|name| self.declarations_by_name.get(name))
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    pub(super) fn intern_declaration_name(&mut self, name: &str) -> DeclarationNameId {
        if let Some(id) = self.declaration_name_ids.get(name) {
            return *id;
        }
        let id = DeclarationNameId::from_raw(next_id(
            self.declaration_names.len(),
            "persistent declaration names",
        ));
        let name: Box<str> = name.into();
        self.declaration_names.push(name.clone());
        self.declaration_name_ids.insert(name, id);
        id
    }

    fn publish_callable(
        &mut self,
        id: CallableId,
        declaration: DeclarationId,
        name: super::body::ResolvedCallableName,
        shape: super::body::ResolvedCallableShape,
        inline: bool,
    ) -> ResolvedCallableHeader {
        assert!(
            self.signatures.contains_key(&declaration),
            "a callable identity requires a pending-free published signature"
        );
        self.insert_callable(id, declaration, name, shape, inline)
    }

    fn insert_callable(
        &mut self,
        id: CallableId,
        declaration: DeclarationId,
        name: super::body::ResolvedCallableName,
        shape: super::body::ResolvedCallableShape,
        inline: bool,
    ) -> ResolvedCallableHeader {
        let callable = ResolvedCallableHeader::new(id, declaration, name, shape, inline);
        if self.diagnostic_callables.remove(&id) {
            assert_eq!(
                self.callable_by_declaration.get(&declaration),
                Some(&id),
                "a diagnostic callable may only upgrade its own declaration"
            );
            assert_eq!(
                self.callables.get(&id).map(|header| header.declaration),
                Some(declaration),
                "a diagnostic callable may only upgrade its own identity"
            );
            self.callable_parameters.remove(&id);
            self.callables.insert(id, callable);
            return callable;
        }
        assert!(
            !self.callable_by_declaration.contains_key(&declaration),
            "a declaration may publish only one callable identity"
        );
        assert!(
            !self.callables.contains_key(&callable.id),
            "a resolved callable identity may be published only once"
        );
        self.callable_by_declaration.insert(declaration, id);
        self.callables.insert(callable.id, callable);
        callable
    }

    /// Retain only a failed function's coordinate-free declaration shape for diagnostic recovery.
    /// No semantic signature accompanies this header, so valid FIR/lowering consumers cannot use
    /// it; [`crate::fir::module_symbols::ModuleSymbols`] projects it transiently with `Ty::Error`
    /// while checking an already-invalid module.
    pub(crate) fn publish_failed_function_shape(
        &mut self,
        id: CallableId,
        declaration: DeclarationId,
        emission_name: &str,
        shape: super::body::ResolvedCallableShape,
    ) -> ResolvedCallableHeader {
        let name = self.intern_declaration_name(emission_name);
        let callable = self.insert_callable(
            id,
            declaration,
            super::body::ResolvedCallableName::Function(name),
            shape,
            false,
        );
        assert!(
            self.diagnostic_callables.insert(id),
            "a failed callable header may be published only once"
        );
        callable
    }

    pub fn publish_function(
        &mut self,
        id: CallableId,
        declaration: DeclarationId,
        emission_name: &str,
        inline: bool,
    ) -> ResolvedCallableHeader {
        self.publish_function_shape(
            id,
            declaration,
            emission_name,
            super::body::ResolvedCallableShape::default(),
            inline,
        )
    }

    pub fn publish_function_shape(
        &mut self,
        id: CallableId,
        declaration: DeclarationId,
        emission_name: &str,
        shape: super::body::ResolvedCallableShape,
        inline: bool,
    ) -> ResolvedCallableHeader {
        let name = self.intern_declaration_name(emission_name);
        self.publish_callable(
            id,
            declaration,
            super::body::ResolvedCallableName::Function(name),
            shape,
            inline,
        )
    }

    pub fn publish_constructor(
        &mut self,
        id: CallableId,
        declaration: DeclarationId,
    ) -> ResolvedCallableHeader {
        self.publish_callable(
            id,
            declaration,
            super::body::ResolvedCallableName::Constructor,
            super::body::ResolvedCallableShape::default(),
            false,
        )
    }

    pub fn publish_constructor_shape(
        &mut self,
        id: CallableId,
        declaration: DeclarationId,
        shape: super::body::ResolvedCallableShape,
    ) -> ResolvedCallableHeader {
        self.publish_callable(
            id,
            declaration,
            super::body::ResolvedCallableName::Constructor,
            shape,
            false,
        )
    }

    pub fn property_declaration(&self, property: PropertyId) -> Option<DeclarationId> {
        self.property(property).map(|property| property.declaration)
    }

    pub fn property(&self, property: PropertyId) -> Option<ResolvedPropertyHeader> {
        self.properties.get(&property).copied()
    }

    pub fn property_for_declaration(&self, declaration: DeclarationId) -> Option<PropertyId> {
        self.property_by_declaration.get(&declaration).copied()
    }

    pub fn publish_property(&mut self, id: PropertyId, declaration: DeclarationId) -> PropertyId {
        self.publish_property_shape(id, declaration, 0, 0, None, false)
    }

    pub fn publish_property_shape(
        &mut self,
        id: PropertyId,
        declaration: DeclarationId,
        context_parameter_count: u32,
        context_value_count: u32,
        extension_receiver: Option<ResolvedTy>,
        mutable: bool,
    ) -> PropertyId {
        assert!(
            context_value_count <= context_parameter_count,
            "named context values cannot exceed context parameters"
        );
        assert!(
            self.signatures.contains_key(&declaration),
            "a property identity requires a pending-free published signature"
        );
        assert!(
            self.property_by_declaration
                .insert(declaration, id)
                .is_none(),
            "a declaration may publish only one property identity"
        );
        assert!(
            self.properties
                .insert(
                    id,
                    ResolvedPropertyHeader {
                        id,
                        declaration,
                        context_parameter_count,
                        context_value_count,
                        extension_receiver,
                        storage_type: None,
                        mutable,
                    },
                )
                .is_none(),
            "a resolved property identity may be published only once"
        );
        id
    }

    pub fn publish_property_storage_type(&mut self, id: PropertyId, storage_type: ResolvedTy) {
        let property = self
            .properties
            .get_mut(&id)
            .expect("a storage type requires a published property identity");
        assert!(
            property.storage_type.replace(storage_type).is_none(),
            "a property storage type may be published only once"
        );
    }

    pub fn publish_property_context_parameter_names(
        &mut self,
        id: PropertyId,
        names: impl IntoIterator<Item = Box<str>>,
    ) {
        let names = names.into_iter().collect::<Vec<_>>().into_boxed_slice();
        let expected = self
            .property(id)
            .expect("context parameter names require a published property")
            .context_parameter_count as usize;
        assert_eq!(
            names.len(),
            expected,
            "property context parameter names must match its resolved signature"
        );
        if names.is_empty() {
            return;
        }
        assert!(
            self.property_context_parameter_names
                .insert(id, names)
                .is_none(),
            "property context parameter names may be published only once"
        );
    }

    pub fn property_context_parameter_name(
        &self,
        property: PropertyId,
        ordinal: u32,
    ) -> Option<&str> {
        self.property_context_parameter_names
            .get(&property)?
            .get(ordinal as usize)
            .map(AsRef::as_ref)
    }

    /// Persistent signature payload only. Temporary graph nodes and source bodies cannot contribute
    /// because neither type is a field of this index.
    pub fn storage_payload_bytes(&self) -> usize {
        self.declarations.storage_payload_bytes()
            + self.source_packages.len()
                * (std::mem::size_of::<SourceFileId>() + std::mem::size_of::<TypeName>())
            + self.source_inventory.len()
                * (std::mem::size_of::<SourceFileId>()
                    + std::mem::size_of::<Box<[DeclarationId]>>())
            + self
                .source_inventory
                .values()
                .map(|declarations| declarations.len() * std::mem::size_of::<DeclarationId>())
                .sum::<usize>()
            + self.source_orders.len()
                * (std::mem::size_of::<DeclarationId>() + std::mem::size_of::<u32>())
            + self.declaration_headers.len()
                * (std::mem::size_of::<DeclarationId>()
                    + std::mem::size_of::<ResolvedDeclarationHeader>())
            + self.local_classifier_lexical_roots.len() * (std::mem::size_of::<DeclarationId>() * 2)
            + self.declaration_annotations.len()
                * (std::mem::size_of::<DeclarationId>() + std::mem::size_of::<Box<[TypeName]>>())
            + self
                .declaration_annotations
                .values()
                .map(|annotations| annotations.len() * std::mem::size_of::<TypeName>())
                .sum::<usize>()
            + self.declaration_annotation_string_arguments.len()
                * (std::mem::size_of::<(DeclarationId, u32)>()
                    + std::mem::size_of::<Box<[Box<str>]>>())
            + self
                .declaration_annotation_string_arguments
                .values()
                .map(|arguments| {
                    arguments.len() * std::mem::size_of::<Box<str>>()
                        + arguments
                            .iter()
                            .map(|argument| argument.len())
                            .sum::<usize>()
                })
                .sum::<usize>()
            + self.visibility_suppressions.len() * std::mem::size_of::<DeclarationId>()
            + self.classifiers.len()
                * (std::mem::size_of::<DeclarationId>()
                    + std::mem::size_of::<ResolvedClassifierHeader>())
            + self.classifier_hierarchies.len()
                * (std::mem::size_of::<DeclarationId>()
                    + std::mem::size_of::<Box<[ResolvedAppliedClassifier]>>())
            + self
                .classifier_hierarchies
                .values()
                .map(|hierarchy| hierarchy.len() * std::mem::size_of::<ResolvedAppliedClassifier>())
                .sum::<usize>()
            + self.property_overrides.len()
                * (std::mem::size_of::<DeclarationId>()
                    + std::mem::size_of::<Box<[super::ResolvedPropertyOverride]>>())
            + self
                .property_overrides
                .values()
                .map(|overrides| {
                    overrides.len() * std::mem::size_of::<super::ResolvedPropertyOverride>()
                })
                .sum::<usize>()
            + self.function_overrides.len()
                * (std::mem::size_of::<DeclarationId>()
                    + std::mem::size_of::<Box<[super::ResolvedFunctionOverride]>>())
            + self
                .function_overrides
                .values()
                .map(|overrides| {
                    overrides.len() * std::mem::size_of::<super::ResolvedFunctionOverride>()
                        + overrides
                            .iter()
                            .map(|edge| {
                                (edge.declared_parameters.len()
                                    + edge.applied_parameters.len()
                                    + edge.implementation_parameters.len())
                                    * std::mem::size_of::<ResolvedTy>()
                            })
                            .sum::<usize>()
                })
                .sum::<usize>()
            + self
                .classifiers
                .values()
                .map(|classifier| classifier.interfaces.len() * std::mem::size_of::<ResolvedTy>())
                .sum::<usize>()
            + self
                .classifiers
                .values()
                .map(|classifier| {
                    classifier.interface_delegations.len()
                        * std::mem::size_of::<ResolvedInterfaceDelegation>()
                        + classifier
                            .interface_delegations
                            .iter()
                            .map(ResolvedInterfaceDelegation::storage_payload_bytes)
                            .sum::<usize>()
                })
                .sum::<usize>()
            + self.type_aliases.len()
                * (std::mem::size_of::<DeclarationId>()
                    + std::mem::size_of::<ResolvedTypeAliasHeader>())
            + self
                .type_aliases
                .values()
                .map(|alias| alias.expansion_spelling.storage_payload_bytes())
                .sum::<usize>()
            + self.signatures.len()
                * (std::mem::size_of::<DeclarationId>() + std::mem::size_of::<ResolvedSignature>())
            + self.contracts.len()
                * (std::mem::size_of::<DeclarationId>()
                    + std::mem::size_of::<crate::contracts::ResolvedContract>())
            + self
                .contracts
                .values()
                .map(crate::contracts::ResolvedContract::storage_payload_bytes)
                .sum::<usize>()
            + self.declaration_spellings.len()
                * (std::mem::size_of::<DeclarationId>()
                    + std::mem::size_of::<crate::spelling::DeclaredSpellings>())
            + self
                .declaration_spellings
                .values()
                .map(crate::spelling::DeclaredSpellings::storage_payload_bytes)
                .sum::<usize>()
            + self.compile_time_constants.len()
                * (std::mem::size_of::<DeclarationId>()
                    + std::mem::size_of::<crate::libraries::LibraryConst>())
            + self.callables.len()
                * (std::mem::size_of::<CallableId>()
                    + std::mem::size_of::<ResolvedCallableHeader>())
            + self.callable_by_declaration.len()
                * (std::mem::size_of::<DeclarationId>() + std::mem::size_of::<CallableId>())
            + self.diagnostic_callables.len() * std::mem::size_of::<CallableId>()
            + self.classifier_type_arguments.len()
                * (std::mem::size_of::<DeclarationId>()
                    + std::mem::size_of::<Box<[TypeParameterId]>>())
            + self
                .classifier_type_arguments
                .values()
                .map(|parameters| parameters.len() * std::mem::size_of::<TypeParameterId>())
                .sum::<usize>()
            + self.classifier_own_type_parameter_counts.len()
                * (std::mem::size_of::<DeclarationId>() + std::mem::size_of::<u32>())
            + self.callable_parameter_storage_payload_bytes()
            + self
                .declaration_names
                .iter()
                .map(|name| name.len())
                .sum::<usize>()
            + self.declaration_name_ids.len()
                * (std::mem::size_of::<Box<str>>() + std::mem::size_of::<DeclarationNameId>())
            + self.declarations_by_name.len()
                * (std::mem::size_of::<DeclarationNameId>()
                    + std::mem::size_of::<Vec<DeclarationId>>())
            + self
                .declarations_by_name
                .values()
                .map(|declarations| declarations.len() * std::mem::size_of::<DeclarationId>())
                .sum::<usize>()
            + self.properties.len()
                * (std::mem::size_of::<PropertyId>() + std::mem::size_of::<DeclarationId>())
            + self.property_by_declaration.len()
                * (std::mem::size_of::<DeclarationId>() + std::mem::size_of::<PropertyId>())
            + self.property_context_parameter_names.len()
                * (std::mem::size_of::<PropertyId>() + std::mem::size_of::<Box<[Box<str>]>>())
            + self
                .property_context_parameter_names
                .values()
                .map(|names| {
                    names.len() * std::mem::size_of::<Box<str>>()
                        + names.iter().map(|name| name.len()).sum::<usize>()
                })
                .sum::<usize>()
            + self.type_parameter_storage_payload_bytes()
            + self
                .signatures
                .values()
                .map(|signature| signature.parameters.len() * std::mem::size_of::<ResolvedTy>())
                .sum::<usize>()
    }
}

/// The complete persistent frontend product. Its type has no field in which a whole AST, temporary
/// signature graph, ordinary FIR body, or whole-module backend IR can be retained.
#[derive(Debug)]
pub struct FrontendModule {
    index: ResolvedModuleIndex,
    inline_bodies: InlineBodyStore,
    default_arguments: DefaultArgumentStore,
    sources: SourceMap,
}

impl FrontendModule {
    pub fn new(
        index: ResolvedModuleIndex,
        inline_bodies: InlineBodyStore,
        default_arguments: DefaultArgumentStore,
        sources: SourceMap,
    ) -> Self {
        Self {
            index,
            inline_bodies,
            default_arguments,
            sources,
        }
    }

    pub fn index(&self) -> &ResolvedModuleIndex {
        &self.index
    }

    pub fn inline_bodies(&self) -> &InlineBodyStore {
        &self.inline_bodies
    }

    pub fn default_arguments(&self) -> &DefaultArgumentStore {
        &self.default_arguments
    }

    pub fn sources(&self) -> &SourceMap {
        &self.sources
    }

    pub fn storage_payload_bytes(&self) -> usize {
        self.index.storage_payload_bytes()
            + self.inline_bodies.storage_payload_bytes()
            + self.default_arguments.storage_payload_bytes()
            + self.sources.storage_payload_bytes()
    }

    pub fn into_parts(
        self,
    ) -> (
        ResolvedModuleIndex,
        InlineBodyStore,
        DefaultArgumentStore,
        SourceMap,
    ) {
        (
            self.index,
            self.inline_bodies,
            self.default_arguments,
            self.sources,
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SignatureFinalizationError {
    /// Required declarations that were not resolved, in the caller's stable declaration order.
    pub declarations: Vec<(DeclarationId, Option<DiagnosticId>)>,
}

/// Force the signature publication boundary. A computing/uncomputed/failed declaration cannot be
/// represented in the returned index, and the input states are consumed so no lazy state travels
/// into FIR checking.
pub fn finalize_signatures(
    required: impl IntoIterator<Item = DeclarationId>,
    states: HashMap<DeclarationId, SignatureState>,
) -> Result<ResolvedModuleIndex, SignatureFinalizationError> {
    let (index, declarations) = finalize_signatures_recovering(required, states);
    if declarations.is_empty() {
        Ok(index)
    } else {
        Err(SignatureFinalizationError { declarations })
    }
}

fn finalize_signatures_recovering(
    required: impl IntoIterator<Item = DeclarationId>,
    mut states: HashMap<DeclarationId, SignatureState>,
) -> (
    ResolvedModuleIndex,
    Vec<(DeclarationId, Option<DiagnosticId>)>,
) {
    let mut index = ResolvedModuleIndex::default();
    let mut declarations = Vec::new();
    for declaration in required {
        match states.remove(&declaration) {
            Some(SignatureState::Resolved(signature)) => {
                index.signatures.insert(declaration, signature);
            }
            Some(SignatureState::Failed(diagnostic)) => {
                declarations.push((declaration, Some(diagnostic)));
            }
            other @ (Some(SignatureState::Uncomputed | SignatureState::Computing) | None) => {
                crate::trace_compiler!(
                    "fir",
                    "signature solver has no result for {declaration:?}: state={}",
                    match other {
                        Some(SignatureState::Uncomputed) => "uncomputed",
                        Some(SignatureState::Computing) => "computing",
                        _ => "absent",
                    },
                );
                declarations.push((declaration, None));
            }
        }
    }
    (index, declarations)
}
