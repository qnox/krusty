//! Callable facts signature finalization publishes for a checked body, and the routing of that
//! body to the inline store or the lowering sink.

use super::super::body_work::BodyWorkItem;
use super::super::header::{BodyOwnerId, CallableId, DeclarationId, DeclarationNameId};
use super::super::retained_bodies::InlineBodyStore;
use super::super::signature::ResolvedTy;
use super::FirBody;

/// Callable facts published by signature finalization. Construction is crate-private so syntax
/// alone cannot claim that a declaration is semantically inline.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResolvedCallableHeader {
    pub id: CallableId,
    pub declaration: DeclarationId,
    pub name: ResolvedCallableName,
    pub shape: ResolvedCallableShape,
    inline: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResolvedCallableName {
    Function(DeclarationNameId),
    Constructor,
}

/// Backend-neutral callable parameter layout. The resolved signature stores context receivers and
/// declared value parameters; the independently selected extension receiver stays here rather than
/// masquerading as a value argument.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ResolvedCallableShape {
    pub context_parameter_count: u32,
    /// Leading context parameters represented as named lexical values. Remaining context
    /// parameters are legacy receiver slots and therefore participate in the implicit-receiver
    /// tower without creating source-visible value bindings.
    pub context_value_count: u32,
    pub extension_receiver: Option<ResolvedTy>,
}

impl ResolvedCallableHeader {
    pub(in crate::fir) const fn new(
        id: CallableId,
        declaration: DeclarationId,
        name: ResolvedCallableName,
        shape: ResolvedCallableShape,
        inline: bool,
    ) -> Self {
        Self {
            id,
            declaration,
            name,
            shape,
            inline,
        }
    }

    pub const fn is_inline(self) -> bool {
        self.inline
    }
}

/// Ordinary bodies cross the frontend boundary by value. Implementations lower and emit during
/// this call; the frontend keeps no body collection alongside the persistent module.
pub trait CheckedBodySink {
    /// Consume a body after the checked-FIR boundary has finalized all nested capture forwarding.
    fn accept_finalized(&mut self, owner: BodyOwnerId, body: FirBody);

    /// Finalization belongs to this boundary, rather than to individual body-kind dispatchers:
    /// constructors, scripts, enum entries, and default fragments have no callable header and must
    /// still obey exactly the same capture-ownership invariant as ordinary functions.
    fn accept(&mut self, owner: BodyOwnerId, mut body: FirBody) {
        assert_eq!(
            body.owner(),
            owner,
            "checked FIR owner must match the consumed body unit"
        );
        body.finalize_capture_forwarding();
        self.accept_finalized(owner, body);
    }
}

/// Route a checked body according to its resolved inline flag. Inline bodies are prepared before
/// callers; all other bodies are immediately consumed by the lowering/backend sink.
pub fn dispatch_checked_body(
    callable: ResolvedCallableHeader,
    work: BodyWorkItem,
    mut body: FirBody,
    inline_bodies: &mut InlineBodyStore,
    ordinary_sink: &mut impl CheckedBodySink,
) {
    assert_eq!(
        callable.declaration, work.declaration,
        "resolved callable must identify the scheduled declaration"
    );
    assert_eq!(
        body.owner(),
        work.owner,
        "checked FIR owner must match its scheduled body unit"
    );
    if callable.is_inline() {
        body.finalize_capture_forwarding();
        inline_bodies.insert(callable, body);
    } else {
        ordinary_sink.accept(work.owner, body);
    }
}
