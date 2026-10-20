//! The checked lexical environment of a local or anonymous classifier's members: the fields its
//! captured values, delegates and receivers live in, and the semantic identities that tell them
//! apart.

use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ClassCaptureBinding {
    pub(crate) owner: DeclarationId,
    pub(crate) field: u32,
    pub(crate) ty: ResolvedTy,
    pub(crate) shared_cell: bool,
    pub(crate) enclosing_depth: u32,
    /// Receiver-tower coordinate used by member bodies of this classifier when this binding is a
    /// captured receiver. `None` for ordinary captured values/delegates.
    pub(crate) semantic_receiver_depth: Option<u32>,
    pub(crate) receiver_source: Option<ClassReceiverCaptureSource>,
    /// Original classifier field whose closure value this field forwards. A directly captured
    /// value has its own `(owner, field)` identity; a transitive recapture preserves the upstream
    /// identity so independently streamed members never join closure operands by source spelling.
    pub(crate) capture_identity: Option<ClassCaptureIdentity>,
    /// Lexical identity of the captured value's binding, which a local function the class calls
    /// maps its capture by: two same-named fields are told apart by it, never by spelling.
    pub(crate) lexical_binding: Option<u32>,
}

/// Semantic closure coordinate. A lexical capture is the classifier field that first stored it.
/// An implicit receiver is the checker's receiver-rung identity, shared by every classifier that
/// captured that rung. Label spelling is not part of either coordinate.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum ClassCaptureIdentity {
    Field { owner: TypeName, field: u32 },
    Receiver(u32),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ClassReceiverCaptureSource {
    pub(crate) enclosing_depth: u32,
    pub(crate) current: bool,
    pub(crate) depth: u32,
}

/// Checked lexical environment required by ordinary members of a local/anonymous classifier
/// declared inside a retained inline/default FIR fragment. It contains semantic FIR identities and
/// types only; no AST ids, source ranges, names used for lookup, or unresolved types survive.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct ClassBodyContext {
    pub(crate) values: HashMap<String, ClassCaptureBinding>,
    /// Every captured value keyed by semantic closure identity. `values` is only the nearest
    /// source-name view used for ordinary reads; two forwarded closures may legitimately capture
    /// different shadowed bindings with the same spelling.
    pub(crate) capture_values: Vec<(String, ClassCaptureBinding)>,
    pub(crate) delegates: HashMap<String, LocalDelegateBinding>,
    pub(crate) callables: HashMap<BodyLocalCallableDeclarationId, (u32, LocalCallableId)>,
    pub(crate) receivers: Vec<ClassCaptureBinding>,
    pub(crate) enclosing_property: Option<PropertyId>,
}

impl ClassBodyContext {
    /// Record a captured value's field. A value a nearer same-named binding shadows where the
    /// class is declared is a field of its own, but not what its name reads in the class body.
    pub(crate) fn record_value(
        &mut self,
        name: String,
        binding: ClassCaptureBinding,
        lexical_shadow_depth: u32,
    ) {
        self.capture_values.push((name.clone(), binding));
        if lexical_shadow_depth == 0 {
            self.values.insert(name, binding);
        }
    }

    pub(crate) fn merge(&mut self, context: Self) {
        self.values.extend(context.values);
        self.capture_values.extend(context.capture_values);
        self.delegates.extend(context.delegates);
        self.callables.extend(context.callables);
        self.receivers.extend(context.receivers);
        self.enclosing_property = context.enclosing_property.or(self.enclosing_property);
    }
}
