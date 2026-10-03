//! Stable identity of one implicit-receiver rung captured by a local or anonymous classifier.
//!
//! The checker's scope identity is an address that does not survive a scope reconstruction. Two
//! classifiers that capture the same rung — a local superclass and the enclosing local class of an inner subclass —
//! must still share one identity, and two rungs that happen to carry the same callable or lambda
//! label must not. A lambda role uses its source span rather than an arena-local AST id, so retained
//! syntax can be reparsed without changing the identity. This table assigns one id per role.

use crate::{diag::Span, types::TypeName};
use std::collections::HashMap;
pub(super) type ReceiverLabel = (
    String,
    crate::types::Ty,
    bool,
    Option<ReceiverDeclarationRole>,
);

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum ReceiverDeclarationRole {
    Lambda {
        expression: Span,
        slot: LambdaReceiverSlot,
    },
    Extension(Span),
    ContextParameter(Span),
    EnclosingClass(TypeName),
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum LambdaReceiverSlot {
    Extension,
    Context(u32),
}

#[derive(Default)]
pub(super) struct ReceiverCaptureIds {
    next: u32,
    assigned: HashMap<ReceiverDeclarationRole, u32>,
}

impl ReceiverCaptureIds {
    pub(super) fn capture_id(
        &mut self,
        class_receiver: bool,
        declaration: Option<ReceiverDeclarationRole>,
    ) -> Option<u32> {
        (!class_receiver).then(|| {
            self.id(declaration
                .expect("a captured non-class receiver has recorded declaration provenance"))
        })
    }

    pub(super) fn id(&mut self, declaration: ReceiverDeclarationRole) -> u32 {
        if let Some(id) = self.assigned.get(&declaration).copied() {
            return id;
        }
        let id = self.next;
        self.next = id
            .checked_add(1)
            .expect("too many implicit receivers in one bounded checker");
        self.assigned.insert(declaration, id);
        id
    }
}

impl super::Checker<'_> {
    /// Closure identity of one declared receiver role. An enclosing class instance is the
    /// classifier's own field, not a closure, so it does not take an id from this table.
    pub(super) fn implicit_receiver_capture_id(
        &mut self,
        class_receiver: bool,
        declaration: Option<ReceiverDeclarationRole>,
    ) -> Option<u32> {
        self.receiver_capture_ids
            .capture_id(class_receiver, declaration)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolve::scope::{ContextReceiver, ContextReceiverKind, Scope, ScopeKind};
    use crate::types::Ty;

    #[test]
    fn rebuilt_scopes_keep_receiver_declaration_identity_at_a_shifted_depth() {
        let root: Scope<'_, ()> = Scope::root();
        let source_lambda = Span { lo: 7, hi: 19 };
        let first = root
            .function_child(Some(Ty::obj("CaptureToken")), None, &[])
            .with_lambda_expression(source_lambda);
        let rebuilt = root
            .function_child(Some(Ty::obj("CaptureToken")), None, &[])
            .with_lambda_expression(source_lambda);
        let nested = rebuilt.child(ScopeKind::Class {
            ty: Ty::obj("CaptureHost"),
            carries_outer: true,
        });
        let first_receiver = first.implicit_receivers_with_declarations()[0];
        let rebuilt_receiver = nested.implicit_receivers_with_declarations()[1];
        assert_ne!(first_receiver.2, rebuilt_receiver.2);
        let first_role = first.implicit_receiver_role(first_receiver.2);
        let rebuilt_role = nested.implicit_receiver_role(rebuilt_receiver.2);
        assert_eq!(
            first_role,
            Some(ReceiverDeclarationRole::Lambda {
                expression: source_lambda,
                slot: LambdaReceiverSlot::Extension,
            })
        );
        assert_eq!(rebuilt_role, first_role);
        let mut ids = ReceiverCaptureIds::default();
        let first_id = ids.capture_id(false, first_role);
        assert_eq!(ids.capture_id(false, rebuilt_role), first_id);
        let unrelated = root
            .function_child(Some(Ty::obj("CaptureToken")), None, &[])
            .with_lambda_expression(Span { lo: 20, hi: 32 });
        let unrelated_identity = unrelated.implicit_receivers_with_declarations()[0].2;
        assert_ne!(
            ids.capture_id(false, unrelated.implicit_receiver_role(unrelated_identity)),
            first_id
        );
    }

    #[test]
    fn a_legacy_receiver_without_declaration_provenance_fails_closed() {
        let root: Scope<'_, ()> = Scope::root();
        let legacy = root.child(ScopeKind::Function {
            receiver: Some(Ty::obj("CaptureToken")),
        });
        let identity = legacy.implicit_receivers_with_declarations()[0].2;
        let role = legacy.implicit_receiver_role(identity);
        assert_eq!(role, None);
        let failure =
            std::panic::catch_unwind(|| ReceiverCaptureIds::default().capture_id(false, role))
                .expect_err("missing receiver declaration is an invalid capture state");
        assert_eq!(
            failure
                .downcast_ref::<String>()
                .map(String::as_str)
                .or_else(|| failure.downcast_ref::<&str>().copied()),
            Some("a captured non-class receiver has recorded declaration provenance")
        );
        assert_eq!(ReceiverCaptureIds::default().capture_id(true, None), None);
    }

    #[test]
    fn declared_receiver_roles_and_lambda_context_slots_are_distinct() {
        let root: Scope<'_, ()> = Scope::root();
        let token = Ty::obj("CaptureToken");
        let context_span = Span { lo: 10, hi: 22 };
        let extension_span = Span { lo: 30, hi: 42 };
        let declared = root.declaration_function_child_with_context(
            Some(token),
            Some((extension_span, "capture".into())),
            &[
                ContextReceiver::new(token, ContextReceiverKind::Anonymous, None)
                    .with_declaration(context_span),
            ],
        );
        let declared_roles = declared
            .implicit_receivers_with_declarations()
            .into_iter()
            .map(|(_, _, identity, _)| declared.implicit_receiver_role(identity))
            .collect::<Vec<_>>();
        assert_eq!(
            declared_roles,
            vec![
                Some(ReceiverDeclarationRole::Extension(extension_span)),
                Some(ReceiverDeclarationRole::ContextParameter(context_span)),
            ]
        );
        let shaped = root
            .function_child(
                Some(token),
                None,
                &[
                    ContextReceiver::named(token, "named"),
                    ContextReceiver::new(token, ContextReceiverKind::FunctionType, None),
                ],
            )
            .with_lambda_expression(Span { lo: 50, hi: 72 })
            .with_current_receiver_context(Some(vec![token, token]));
        let shaped_roles = shaped
            .implicit_receivers_with_declarations()
            .into_iter()
            .map(|(_, _, identity, _)| shaped.implicit_receiver_role(identity))
            .collect::<Vec<_>>();
        assert_eq!(
            shaped_roles,
            vec![
                Some(ReceiverDeclarationRole::Lambda {
                    expression: Span { lo: 50, hi: 72 },
                    slot: LambdaReceiverSlot::Context(2),
                }),
                Some(ReceiverDeclarationRole::Lambda {
                    expression: Span { lo: 50, hi: 72 },
                    slot: LambdaReceiverSlot::Context(1),
                }),
            ]
        );
        let mut ids = ReceiverCaptureIds::default();
        assert_eq!(
            declared_roles
                .into_iter()
                .chain(shaped_roles)
                .map(|role| ids.capture_id(false, role))
                .collect::<Vec<_>>(),
            vec![Some(0), Some(1), Some(2), Some(3)]
        );
    }
}
