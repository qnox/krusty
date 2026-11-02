//! Checked receiver coordinates on either side of an anonymous constructor header.

use super::{CheckerScope, ImplicitReceiver, ReceiverDeclarationRole};
use crate::{ast::DeclId, fx_hash::HashMap};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ReceiverCoordinate {
    pub(crate) current: bool,
    pub(crate) depth: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ConstructorReceiverProjection {
    pub(crate) declaration: DeclId,
    header: ReceiverCoordinate,
    construction: ReceiverCoordinate,
}

impl ConstructorReceiverProjection {
    /// Receivers introduced inside the argument remain above the selected declared receiver.
    pub(crate) fn project(&self, depth: usize) -> Option<ReceiverCoordinate> {
        let inner = depth.checked_sub(self.header.depth)?;
        Some(ReceiverCoordinate {
            current: inner == 0 && self.construction.current,
            depth: inner.checked_add(self.construction.depth)?,
        })
    }
}

pub(super) struct ConstructorReceiverFrame {
    receivers: HashMap<ReceiverDeclarationRole, ConstructorReceiverProjection>,
}

impl ConstructorReceiverFrame {
    pub(super) fn new(
        declaration: DeclId,
        header: &CheckerScope<'_>,
        construction: &CheckerScope<'_>,
    ) -> Self {
        let construction = declared_coordinates(construction);
        let receivers = declared_coordinates(header)
            .into_iter()
            .filter_map(|(role, header)| {
                Some((
                    role,
                    ConstructorReceiverProjection {
                        declaration,
                        header,
                        construction: *construction.get(&role)?,
                    },
                ))
            })
            .collect();
        Self { receivers }
    }

    pub(super) fn selected(
        &self,
        receiver: ImplicitReceiver,
    ) -> Option<ConstructorReceiverProjection> {
        self.receivers.get(&receiver.receiver_role?).cloned()
    }
}

pub(super) fn scoped_construction_sites(
    frames: &[ConstructorReceiverFrame],
    scope: &CheckerScope<'_>,
    depth: usize,
) -> Vec<ConstructorReceiverProjection> {
    let role = declared_coordinates(scope)
        .into_iter()
        .find_map(|(role, coordinate)| (coordinate.depth == depth).then_some(role));
    role.map(|role| {
        frames
            .iter()
            .filter_map(|frame| frame.receivers.get(&role).cloned())
            .collect()
    })
    .unwrap_or_default()
}

fn declared_coordinates(
    scope: &CheckerScope<'_>,
) -> HashMap<ReceiverDeclarationRole, ReceiverCoordinate> {
    scope
        .implicit_receivers_with_declarations()
        .into_iter()
        .enumerate()
        .filter_map(|(depth, (ty, _, identity, class))| {
            if scope.implicit_receiver_is_singleton(identity) {
                return None;
            }
            let role = if class {
                ReceiverDeclarationRole::EnclosingClass(ty.non_null().kotlin_class_internal()?)
            } else {
                scope.implicit_receiver_role(identity)?
            };
            Some((
                role,
                ReceiverCoordinate {
                    current: depth == 0,
                    depth,
                },
            ))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constructor_receiver_frames_distinguish_same_typed_declaration_roles() {
        let root = CheckerScope::root();
        let outer = root
            .function_child(Some(crate::types::Ty::obj("Token")), None, &[])
            .with_lambda_expression(crate::diag::Span { lo: 7, hi: 19 });
        let inner = outer
            .function_child(Some(crate::types::Ty::obj("Token")), None, &[])
            .with_lambda_expression(crate::diag::Span { lo: 20, hi: 32 });
        let dispatch = inner.dispatch_receiver_child(crate::types::Ty::obj("Anonymous"));
        let header = dispatch.singleton_receiver_child(Some(crate::types::Ty::obj("Companion")));
        let frames = [ConstructorReceiverFrame::new(DeclId(3), &header, &inner)];
        let nearer = scoped_construction_sites(&frames, &header, 2);
        let further = scoped_construction_sites(&frames, &header, 3);
        assert_eq!(nearer.len(), 1);
        assert_eq!(further.len(), 1);
        assert_eq!(
            nearer[0].project(2),
            Some(ReceiverCoordinate {
                current: true,
                depth: 0
            })
        );
        assert_eq!(
            further[0].project(3),
            Some(ReceiverCoordinate {
                current: false,
                depth: 1
            })
        );
        assert!(scoped_construction_sites(&frames, &header, 0).is_empty());
    }
}
