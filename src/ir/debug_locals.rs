//! Backend-neutral identity carried for source and inline debug locals.

use crate::types::TypeName;

use super::FunId;

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
            Self::InlineLambdaReceiver { .. } => self,
        }
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
