//! The checked context one source unit's body callbacks share.

use std::collections::HashMap;

use crate::fir::{
    ActiveSourceDeclarations, BodyLocalCallableDeclarationId, ClassBodyContext, DeclarationId,
    FirBody, FirCapture, FirImplicitReceiverCapture, FirStatement, FirStatementId,
    FirStatementKind,
};

/// Transient checked context shared only by body callbacks for the currently active source unit.
/// Any Pass-1 context needed later is owned by its retained inline/default [`FirBody`] and copied
/// into a fresh session at the start of Pass 2; the session itself never crosses the pass boundary.
#[derive(Default)]
pub struct BodyCheckSession {
    pub(super) class_bodies: HashMap<DeclarationId, ClassBodyContext>,
    pub(super) local_callables: HashMap<BodyLocalCallableDeclarationId, PublishedLocalCallable>,
    /// The captured values of each local function checked so far in this unit, in the order its
    /// lifted method takes them. A later local function that calls it takes them in that order.
    pub(super) local_function_captures: HashMap<BodyLocalCallableDeclarationId, Box<[FirCapture]>>,
    pub(super) local_delegated_properties:
        crate::fir::local_delegated_properties::LocalDelegatedPropertyIds,
    pub(super) active_source: Option<ActiveSourceDeclarations>,
}

#[derive(Clone, Debug)]
pub(super) struct PublishedLocalCallable {
    pub(super) captures: Box<[FirCapture]>,
    /// For each capture, the position the resolver declared it at.
    pub(super) declaration_ordinals: Box<[u32]>,
    pub(super) implicit_receiver_captures: Box<[FirImplicitReceiverCapture]>,
}

impl BodyCheckSession {
    pub(super) fn install_active_source(&mut self, active: &ActiveSourceDeclarations) {
        // Pass 2 binds a fresh parser arena for every top-level declaration unit. Two successive
        // bindings can belong to the same source file while assigning the same transient DeclId to
        // different stable local classifiers, so source identity alone cannot make this cacheable.
        self.active_source = Some(active.clone());
    }

    pub(crate) fn absorb_retained_body(&mut self, body: &FirBody) {
        body.collect_class_body_contexts(&mut self.class_bodies);
        self.absorb_checked_body(body);
    }

    pub(super) fn absorb_checked_body(&mut self, body: &FirBody) {
        for raw in 0..body.statement_count() {
            let statement = FirStatementId::from_raw(
                u32::try_from(raw).expect("too many FIR statements for a packed identity"),
            );
            let Some(FirStatement {
                kind:
                    FirStatementKind::LocalFunction {
                        declaration, body, ..
                    },
                ..
            }) = body.statement(statement)
            else {
                continue;
            };
            let published = PublishedLocalCallable {
                captures: body.captures().to_vec().into_boxed_slice(),
                declaration_ordinals: body.capture_declaration_ordinals().into(),
                implicit_receiver_captures: body
                    .implicit_receiver_captures()
                    .to_vec()
                    .into_boxed_slice(),
            };
            if let Some(previous) = self.local_callables.insert(*declaration, published.clone()) {
                debug_assert_eq!(previous.captures, published.captures);
                debug_assert_eq!(
                    previous.implicit_receiver_captures,
                    published.implicit_receiver_captures
                );
            }
            self.absorb_checked_body(body);
        }
    }
}
