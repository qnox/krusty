//! Backend-neutral identity carried for source and inline debug locals.

use crate::types::TypeName;

use super::{ExprId, FunId, IrFile};

impl IrFile {
    pub(crate) fn set_debug_local_provenance(
        &mut self,
        declaration: ExprId,
        provenance: IrDebugLocalProvenance,
    ) {
        self.debug_local_provenance.insert(declaration, provenance);
    }

    pub(crate) fn debug_local_provenance(
        &self,
        declaration: ExprId,
    ) -> Option<IrDebugLocalProvenance> {
        self.debug_local_provenance.get(&declaration).copied()
    }
}

/// Backend-neutral origin of a debug-visible local materialized while expanding an inline body.
/// Common lowering retains the source name separately in `IrFile::value_names` and records only
/// the semantic role/depth here. A target owns separators, escaping, and synthetic spelling.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum IrDebugLocalProvenance {
    InlineValue {
        role: IrInlineLocalRole,
        depth: u32,
    },
    /// Receiver parameter of a source lambda whose body was spliced at an inline call site. The
    /// stable function identity leads to [`IrLambdaOrigin`]; no generated method name crosses the
    /// common-IR boundary.
    InlineLambdaReceiver {
        implementation: FunId,
    },
    /// A value parameter of a source lambda spliced at an inline call site, bound from the call's
    /// operand. kotlinc keeps the parameter's source spelling (`it` stays `it` — the `$iv` suffix
    /// is for the inlined callable's OWN locals); `depth` counts the enclosing expansions the
    /// splice was cloned into, each of which adds one suffix, as a lambda frame marker's does.
    InlineLambdaParameter {
        depth: u32,
    },
    /// Invocation parameter of a callable reference consumed directly by an inline call. The
    /// ordinal is semantic provenance; each backend owns the physical `pN`-style spelling.
    InlineCallableReferenceParameter {
        ordinal: u32,
    },
    /// The inline-depth marker an inline function's expansion opens with, after its operands are
    /// bound. The retained source name is the expanded callable's. It never grows with nesting: a
    /// debugger reads it as the name of the frame it opens, wherever that frame was cloned to.
    FunctionFrameMarker,
    /// The inline-depth marker a spliced lambda body opens with, after its parameters are bound.
    /// The retained source name is the inline callable the lambda was passed to; `implementation`
    /// leads to the lambda's [`IrLambdaOrigin`]. `depth` counts the enclosing expansions the
    /// splice was cloned into, as an ordinary inline value's does.
    LambdaFrameMarker {
        implementation: FunId,
        depth: u32,
    },
}

impl IrDebugLocalProvenance {
    pub(crate) fn inline_value(role: IrInlineLocalRole, depth: u32) -> Self {
        debug_assert!(depth > 0, "an inline expansion depth is one-based");
        Self::InlineValue { role, depth }
    }

    pub(crate) fn nested_inline(self) -> Self {
        match self {
            Self::InlineValue { role, depth } => Self::InlineValue {
                role,
                depth: depth.saturating_add(1),
            },
            Self::LambdaFrameMarker {
                implementation,
                depth,
            } => Self::LambdaFrameMarker {
                implementation,
                depth: depth.saturating_add(1),
            },
            Self::InlineLambdaParameter { depth } => Self::InlineLambdaParameter {
                depth: depth.saturating_add(1),
            },
            Self::InlineLambdaReceiver { .. }
            | Self::InlineCallableReferenceParameter { .. }
            | Self::FunctionFrameMarker => self,
        }
    }

    /// Whether this local is an inline-depth marker: a frame boundary for a debugger, never a
    /// value any code reads.
    pub(crate) fn is_inline_marker(self) -> bool {
        matches!(
            self,
            Self::FunctionFrameMarker | Self::LambdaFrameMarker { .. }
        )
    }
}

/// What a local materialized by an inline expansion IS to the expanded callable. A member inline
/// EXTENSION binds both receivers at once, and Kotlin keeps them distinct — the containing class's
/// `this` and the extension's receiver are different values with different debug identities — so
/// one receiver role cannot stand for the other.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum IrInlineLocalRole {
    Value,
    /// The containing class's `this`, bound because the inline callable is a member.
    DispatchReceiver,
    /// The receiver the inline callable extends.
    ExtensionReceiver,
}

/// Stable source identity and lexical naming context for one lowered lambda implementation. A
/// source lambda can be lowered more than once (for example into multiple constructors); every such
/// implementation carries the same origin so a backend can realize one closure artifact without
/// recovering identity from generated method names or scanning unrelated expression/value tables.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IrLambdaOrigin {
    /// File-local semantic identity assigned while a checked source lambda is consumed. It is not
    /// an AST id, text offset, or body locator.
    pub identity: u32,
    /// Semantic classifier whose lexical code container owns the implementation, or `None` for the
    /// package facade. Physical method placement may still be changed by a backend pass.
    pub lexical_owner: Option<TypeName>,
    pub enclosing_name: String,
    pub binding_name: Option<String>,
    /// Source-lambda ordinal for class-mode naming within the rendered lexical context.
    pub ordinal: u32,
    /// Backend-neutral source naming stem of the containing executable declaration. A target owns
    /// the separators and complete physical implementation spelling.
    pub implementation_name: String,
    /// Source-lambda implementation ordinal within the enclosing callable name.
    pub implementation_ordinal: u32,
    /// Parameter position of the source lambda's extension receiver in its common implementation
    /// signature. Captures and context receivers precede it. This semantic coordinate lets inline
    /// expansion preserve the receiver without recognizing the debug placeholder `<this>`.
    pub receiver_parameter: Option<u32>,
    /// Source spelling of a lambda literal's label: its own, else the call it is written in.
    /// `None` for a lambda outside every call, or in a scope that names none (a property
    /// initializer, an assignment, a non-infix operator), and for an anonymous function.
    pub label: Option<String>,
    /// Which source form the implementation was written in.
    pub form: IrLambdaForm,
    /// Whether the source expression carried the `suspend` modifier. This is distinct from the
    /// inferred function type: Kotlin metadata marks an explicit `suspend { ... }` function and
    /// leaves a lambda merely coerced to a suspend function type unmarked.
    pub explicit_suspend: bool,
    /// The naming walk's position for the class the lambda would compile to. A lambda spliced into
    /// an inline call writes no class, but a target spells its inline-depth marker from this
    /// provenance. The realized spelling stays in that target's own state.
    pub(crate) class_provenance: Option<super::IrLocalClassNameProvenance>,
}

impl IrFile {
    /// Whether `function` implements a lambda literal, rather than an anonymous function or a
    /// declaration.
    pub fn is_lambda_literal(&self, function: u32) -> bool {
        self.lambda_origins
            .get(&function)
            .is_some_and(|origin| origin.form == IrLambdaForm::Literal)
    }
}

/// The source form of a lowered function literal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IrLambdaForm {
    /// A lambda literal (`{ … }`).
    Literal,
    /// An anonymous function expression (`fun(…) { … }`).
    AnonymousFunction,
}

/// A `catch (e: E)` parameter's debug-local facts.
///
/// A catch binding is DECLARED by its `IrCatch`, not by a `Variable` node, so it has no declaration
/// expression for `IrFile::value_names` and `IrFile::debug_local_provenance` to be keyed by. It
/// carries the same two facts they hold for every other local, in the record that declares it, and
/// a target renders it through the same boundary rather than writing the source spelling out.
#[derive(Clone, Debug)]
pub struct IrCatchBinding {
    /// Source spelling, as the declaration wrote it.
    pub name: String,
    /// Inline provenance. `None` while the binding is still in the body that declared it; an
    /// expansion that clones this catch nests it exactly as it nests an ordinary local's.
    pub(crate) provenance: Option<IrDebugLocalProvenance>,
}

impl IrCatchBinding {
    pub(crate) fn source(name: String) -> Self {
        Self {
            name,
            provenance: None,
        }
    }

    /// Carry this binding one inline expansion deeper. A binding with no provenance yet is one
    /// this expansion is the first to clone, so it starts at depth one like any copied local.
    pub(crate) fn nest_inline(&mut self) {
        self.provenance = Some(self.provenance.map_or_else(
            || IrDebugLocalProvenance::inline_value(IrInlineLocalRole::Value, 1),
            IrDebugLocalProvenance::nested_inline,
        ));
    }
}
