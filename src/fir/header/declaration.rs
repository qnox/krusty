//! Compact declaration signatures and their retained diagnostic source facts.

use super::{
    DeclarationId, HeaderInterfaceDelegationRange, HeaderParameterAnnotationClassLiteralRange,
    HeaderParameterRange, HeaderTypeBoundRange, HeaderTypeId, HeaderTypeParameterRange,
    HeaderTypeRange, LookupNameId, Span,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HeaderResultType {
    Explicit(HeaderTypeId),
    /// Block-body functions with no written return type are semantically `Unit` and need no graph.
    ImplicitUnit,
    /// Expression-body functions and expression-derived properties are attached to `SignatureGraph`.
    Inferred,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HeaderDeclarationKind {
    Callable {
        receiver: Option<HeaderTypeId>,
        parameters: HeaderParameterRange,
        result: HeaderResultType,
        type_parameters: HeaderTypeParameterRange,
        bounds: HeaderTypeBoundRange,
        context_count: u32,
        /// Start of the declaration signature, used only to derive stable semantic identities for
        /// callable type parameters after the parser-owned declaration has been released.
        signature_start: u32,
        /// End of the declaration signature. Together with `signature_start`, this preserves the
        /// exact diagnostic origin without retaining the declaration or body AST.
        signature_end: u32,
    },
    Property {
        receiver: Option<HeaderTypeId>,
        context_parameters: HeaderParameterRange,
        /// Exact source name of a written setter parameter. An absent/default setter has no source
        /// parameter declaration and is projected separately by each target.
        setter_parameter_name: Option<LookupNameId>,
        declared_type: Option<HeaderTypeId>,
        getter_type: Option<HeaderTypeId>,
        backing_field_type: Option<HeaderTypeId>,
        type_parameters: HeaderTypeParameterRange,
        bounds: HeaderTypeBoundRange,
        mutable: bool,
    },
    Classifier {
        /// Exact source declaration segment retained before parser hoisting gives a nested
        /// classifier its qualified lookup/emission path. Recursive header scope consumes this
        /// spelling directly; it must not recover a segment from `DeclarationStub::lookup_name`.
        source_name: LookupNameId,
        type_parameters: HeaderTypeParameterRange,
        /// Enclosing declaration parameters used by a parser-hoisted local/anonymous classifier.
        /// These bind existing semantic declarations; they are not formals owned by this classifier.
        lexical_type_parameter_captures: HeaderTypeParameterRange,
        bounds: HeaderTypeBoundRange,
        /// Declared interface/function supertypes in source order.
        supertypes: HeaderTypeRange,
        /// The one superclass constructor target, kept distinct from interfaces so later semantic
        /// graph construction never recovers class-vs-interface ownership from source spelling.
        base: Option<HeaderTypeId>,
        /// The callee name of the superclass constructor call (`Base` in `: pkg.Base(args)`).
        /// The call names its classifier apart from `base`, and kotlinc checks its access there.
        base_callee: Option<crate::diag::Span>,
        /// Class context parameters in source order. They are shared by every constructor and are
        /// implicit receivers of instance bodies.
        context_parameters: HeaderParameterRange,
        primary_parameters: HeaderParameterRange,
        delegations: HeaderInterfaceDelegationRange,
    },
    Constructor {
        context_parameters: HeaderParameterRange,
        parameters: HeaderParameterRange,
    },
    TypeAlias {
        type_parameters: HeaderTypeParameterRange,
        target: HeaderTypeId,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HeaderDeclaration {
    pub declaration: DeclarationId,
    /// Exact written declaration name for diagnostics after the source AST is released.
    /// Constructors have no ordinary declaration name.
    pub name_span: Option<Span>,
    /// Declaration annotations as compact classifier-reference types. Resolution consumes this
    /// temporary range during Pass 1 and publishes stable classifier identities.
    pub annotations: HeaderTypeRange,
    /// Class-literal arguments written on those annotations, keyed by annotation ordinal — the
    /// declaration-level twin of [`crate::fir::HeaderParameter::annotation_class_literals`]. A class-level
    /// `@Serializable(with = X::class)` is a semantic fact another file of the same module must be
    /// able to read, and it cannot be recovered once the source AST is released.
    pub annotation_class_literals: HeaderParameterAnnotationClassLiteralRange,
    pub kind: HeaderDeclarationKind,
}
