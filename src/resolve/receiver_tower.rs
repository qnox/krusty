//! The checker receiver tower and recorded receiver reconstruction for isolated class bodies.

use super::*;

fn enclosing_class_role(ty: Ty) -> ReceiverDeclarationRole {
    ReceiverDeclarationRole::EnclosingClass(
        ty.kotlin_class_internal()
            .expect("an enclosing receiver has a classifier identity"),
    )
}

/// Insert one recorded runtime receiver at its checked coordinate. A removed unavailable class
/// receiver can leave a reserved coordinate, so `receiver_depth` is not a `Vec` index. Keep the
/// vector in lookup order and shift only materialized runtime receivers; lexical singleton
/// sentinels are outside that coordinate space.
fn insert_runtime_receiver(receivers: &mut Vec<ImplicitReceiver>, mut receiver: ImplicitReceiver) {
    let receiver_depth = receiver.receiver_depth;
    assert_ne!(
        receiver_depth,
        usize::MAX,
        "a recorded runtime receiver has a checked coordinate"
    );
    // Find the insertion anchor before shifting coordinates. Existing vector order is the exact
    // lookup order, including scoped singleton receivers whose non-capture sentinel deliberately
    // is not a runtime coordinate. Anchoring on the next runtime rung keeps a nearer/middle
    // singleton on its original side of the reconstructed receiver.
    let position = receivers
        .iter()
        .position(|existing| {
            existing.receiver_depth != usize::MAX && existing.receiver_depth >= receiver_depth
        })
        .unwrap_or(receivers.len());
    for existing in receivers.iter_mut().filter(|existing| {
        existing.receiver_depth != usize::MAX && existing.receiver_depth >= receiver_depth
    }) {
        existing.receiver_depth += 1;
        existing.current = false;
    }
    receiver.current = position == 0;
    receivers.insert(position, receiver);
}

fn complete_class_receivers(
    receivers: &mut Vec<ImplicitReceiver>,
    class_receivers: impl IntoIterator<Item = (usize, Ty, usize)>,
) {
    for (label_index, ty, receiver_depth) in class_receivers {
        let role = enclosing_class_role(ty);
        if receivers
            .iter()
            .any(|receiver| receiver.class_receiver && receiver.receiver_role == Some(role))
        {
            continue;
        }
        insert_runtime_receiver(
            receivers,
            ImplicitReceiver {
                receiver_role: Some(role),
                ty,
                declared_ty: ty,
                identity: receiver_label_identity(label_index),
                extension_receiver: None,
                class_receiver: true,
                current: receiver_depth == 0,
                receiver_depth,
            },
        );
    }
}

pub(super) fn declared_receivers(
    checker: &Checker<'_>,
    scope: &CheckerScope<'_>,
) -> Vec<ImplicitReceiver> {
    // Scope owns receiver ordering. Do not reconstruct it from labels or deduplicate equal types:
    // two same-typed receivers remain distinct runtime values and the selected depth is the exact
    // identity handed to lowering.
    let mut receivers = scope
        .implicit_receivers_with_declarations()
        .into_iter()
        .enumerate()
        .map(
            |(coordinate, (ty, extension_receiver, identity, class_receiver))| {
                let singleton_receiver = scope.implicit_receiver_is_singleton(identity);
                ImplicitReceiver {
                    receiver_role: if class_receiver {
                        Some(enclosing_class_role(ty))
                    } else {
                        scope.implicit_receiver_role(identity)
                    },
                    ty,
                    declared_ty: ty,
                    identity,
                    extension_receiver,
                    class_receiver,
                    current: coordinate == 0,
                    receiver_depth: if singleton_receiver {
                        usize::MAX
                    } else {
                        coordinate
                    },
                }
            },
        )
        .collect::<Vec<_>>();
    let unavailable_class_receiver = (checker.this_unavailable
        && checker.static_companion_this.is_none()
        && checker.static_singleton_this.is_none())
    .then(|| {
        checker
            .this_labels
            .iter()
            .rev()
            .find(|(_, _, is_class, _)| *is_class)
            .map(|(_, ty, _, _)| *ty)
    })
    .flatten();
    // A superclass-constructor argument is evaluated before the current class instance exists.
    // Remove that exact class-rung identity, not the first same-typed receiver: a receiver lambda
    // may introduce another `Outer.Inner` immediately above it, and that value remains valid.
    if unavailable_class_receiver.is_some() {
        let identity = scope
            .innermost_class_receiver_identity()
            .expect("an unavailable class receiver must have a class scope rung");
        receivers.retain(|receiver| receiver.identity != identity);
    }
    // A retained local/anonymous classifier can be checked independently from the expression
    // that introduced it. Its captured receiver labels carry the already-selected semantic
    // types and exact declaration roles. Reinstall a missing rung,
    // or repair a provisional `Pending` receiver on a live postponed-inference scope, without
    // changing an existing runtime identity.
    if let Some(top) = checker.this_labels.len().checked_sub(1) {
        for (index, (_, ty, is_class, role)) in checker.this_labels.iter().enumerate().rev() {
            if *is_class || ty.mentions_pending() {
                continue;
            }
            let Some(role) = role else {
                continue;
            };
            if let Some(receiver) = receivers
                .iter_mut()
                .find(|receiver| receiver.receiver_role == Some(*role))
            {
                if receiver.ty.mentions_pending() {
                    receiver.ty = *ty;
                }
            } else {
                let depth = top - index;
                // A live class/dispatch rung can already occupy this coordinate. The retained
                // declaration role is nearer in the original tower, so insert it and shift only
                // runtime-coordinate receivers. Singleton entries use the sentinel depth and are
                // materialized independently.
                insert_runtime_receiver(
                    &mut receivers,
                    ImplicitReceiver {
                        receiver_role: Some(*role),
                        ty: *ty,
                        declared_ty: *ty,
                        identity: (0, depth),
                        extension_receiver: None,
                        class_receiver: false,
                        current: depth == 0,
                        receiver_depth: depth,
                    },
                );
            }
        }
    }
    // Nested declarations are stored flat in the AST, so an inner class's `ScopeKind::Class`
    // rung carries its current receiver while `this_labels` carries the semantic `inner_of`
    // chain. Complete that same receiver tower here. These are runtime receivers (unlike the
    // lexical-classifier fall-through below), and their ordinal is the exact `this$0` walk that
    // lowering consumes. Do not deduplicate equal types: recursively nested instances of the
    // same classifier are distinct values.
    let class_receivers = checker
        .this_labels
        .iter()
        .enumerate()
        .rev()
        .filter(|(index, (label, ty, is_class, _))| {
            *is_class
                || (*index + 1 < checker.this_labels.len()
                    && ty
                        .obj_internal()
                        .is_some_and(|owner| checker.classifier_has_enum_entry(owner, label)))
        })
        .skip(usize::from(unavailable_class_receiver.is_some()))
        .map(|(label_index, (_, ty, _, _))| {
            let receiver_depth = checker
                .this_labels
                .len()
                .checked_sub(label_index + 1)
                .expect("an enclosing class label has a receiver coordinate");
            (label_index, *ty, receiver_depth)
        })
        .collect::<Vec<_>>();
    complete_class_receivers(&mut receivers, class_receivers);
    // A declaration nested in an `object` does not capture an outer instance, but the lexical
    // singleton is still an implicit receiver in Kotlin. Add only resolved object classifiers
    // from the lexical owner chain; ordinary nested classes remain a receiver cut. Lowering can
    // materialize this exact selection from the object's `INSTANCE`, so no storage inference or
    // name-based retry is involved.
    for owner in checker.lexical_source_class_names() {
        let receiver = Ty::obj_name(owner);
        if checker.classifier_is_object(owner)
            && !receivers
                .iter()
                .any(|existing: &ImplicitReceiver| existing.ty == receiver)
        {
            receivers.push(ImplicitReceiver {
                receiver_role: None,
                ty: receiver,
                declared_ty: receiver,
                identity: (0, receivers.len()),
                extension_receiver: None,
                class_receiver: false,
                current: false,
                receiver_depth: usize::MAX,
            });
        }
    }
    // Companions of the lexically enclosing classes AND of everything they inherit: a subclass
    // reaches its superclass's companion members by bare name (`open class A { companion object
    // { fun getO() } }; class C : A() { fun t() = getO() }`). Nearest first — the own companion
    // outranks an inherited one — and every rung is kept, not just the first class that has one,
    // because two levels of the chain may each declare different members.
    let source = checker.fed_source();
    let companions = checker
        .lexical_source_class_names()
        .into_iter()
        .flat_map(|owner| {
            crate::symbol_resolver::applied_hierarchy(&source, Ty::obj_name(owner))
                .into_iter()
                .map(|(inherited, _, _)| inherited)
        })
        .filter_map(|owner| {
            source.classifier(owner).and_then(|class| {
                class
                    .companion_object
                    .as_ref()
                    .map(|(_, companion)| *companion)
            })
        })
        .collect::<Vec<_>>();
    for companion in companions {
        let ty = Ty::obj_name(companion);
        if !receivers.iter().any(|receiver| receiver.ty == ty) {
            receivers.push(ImplicitReceiver {
                receiver_role: None,
                ty,
                declared_ty: ty,
                identity: (0, receivers.len()),
                extension_receiver: None,
                class_receiver: false,
                current: false,
                receiver_depth: usize::MAX,
            });
        }
    }
    receivers
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diag::Span;

    fn runtime_receiver(
        ty: Ty,
        role: Option<ReceiverDeclarationRole>,
        class_receiver: bool,
        receiver_depth: usize,
    ) -> ImplicitReceiver {
        ImplicitReceiver {
            receiver_role: role,
            ty,
            declared_ty: ty,
            identity: (1, receiver_depth),
            extension_receiver: None,
            class_receiver,
            current: receiver_depth == 0,
            receiver_depth,
        }
    }

    fn lambda_role() -> ReceiverDeclarationRole {
        ReceiverDeclarationRole::Lambda {
            expression: Span { lo: 7, hi: 19 },
            slot: LambdaReceiverSlot::Extension,
        }
    }

    #[test]
    fn recorded_receiver_uses_checked_order_after_an_unavailable_class_gap() {
        let outer = Ty::obj("CaptureOuter");
        let mut receivers = vec![runtime_receiver(
            outer,
            Some(enclosing_class_role(outer)),
            true,
            1,
        )];
        let lambda = Ty::obj("CaptureToken");
        insert_runtime_receiver(
            &mut receivers,
            runtime_receiver(lambda, Some(lambda_role()), false, 1),
        );

        assert_eq!(receivers.len(), 2);
        assert_eq!(receivers[0].receiver_role, Some(lambda_role()));
        assert_eq!(receivers[0].receiver_depth, 1);
        assert_eq!(
            receivers[1].receiver_role,
            Some(enclosing_class_role(outer))
        );
        assert_eq!(receivers[1].receiver_depth, 2);
    }

    #[test]
    fn class_completion_matches_exact_roles_across_a_recorded_receiver() {
        let current = Ty::obj("CaptureCurrent");
        let outer = Ty::obj("CaptureOuter");
        let token = Ty::obj("CaptureToken");
        let mut receivers = vec![
            runtime_receiver(current, Some(enclosing_class_role(current)), true, 0),
            runtime_receiver(token, Some(lambda_role()), false, 1),
            runtime_receiver(outer, Some(enclosing_class_role(outer)), true, 2),
        ];

        complete_class_receivers(&mut receivers, [(2, current, 0), (0, outer, 2)]);

        assert_eq!(receivers.len(), 3);
        assert_eq!(
            receivers[0].receiver_role,
            Some(enclosing_class_role(current))
        );
        assert_eq!(receivers[1].receiver_role, Some(lambda_role()));
        assert_eq!(
            receivers[2].receiver_role,
            Some(enclosing_class_role(outer))
        );
        assert_eq!(
            receivers
                .iter()
                .filter(|receiver| receiver.receiver_role == Some(enclosing_class_role(outer)))
                .count(),
            1
        );
    }

    #[test]
    fn runtime_insertion_keeps_a_nearest_scoped_singleton_first() {
        let singleton = Ty::obj("CaptureSingleton");
        let outer = Ty::obj("CaptureOuter");
        let mut singleton_receiver = runtime_receiver(singleton, None, false, usize::MAX);
        singleton_receiver.current = true;
        let mut receivers = vec![
            singleton_receiver,
            runtime_receiver(outer, Some(enclosing_class_role(outer)), true, 1),
        ];
        let token = Ty::obj("CaptureToken");
        insert_runtime_receiver(
            &mut receivers,
            runtime_receiver(token, Some(lambda_role()), false, 1),
        );

        assert_eq!(receivers.len(), 3);
        assert_eq!(receivers[0].ty, singleton);
        assert!(receivers[0].current);
        assert_eq!(receivers[0].receiver_depth, usize::MAX);
        assert_eq!(receivers[1].receiver_role, Some(lambda_role()));
        assert_eq!(receivers[1].receiver_depth, 1);
        assert_eq!(receivers[2].receiver_depth, 2);
    }

    #[test]
    fn runtime_insertion_preserves_a_middle_scoped_singleton_rung() {
        let current = Ty::obj("CaptureCurrent");
        let singleton = Ty::obj("CaptureSingleton");
        let outer = Ty::obj("CaptureOuter");
        let mut receivers = vec![
            runtime_receiver(current, Some(enclosing_class_role(current)), true, 0),
            runtime_receiver(singleton, None, false, usize::MAX),
            runtime_receiver(outer, Some(enclosing_class_role(outer)), true, 2),
        ];
        let token = Ty::obj("CaptureToken");
        insert_runtime_receiver(
            &mut receivers,
            runtime_receiver(token, Some(lambda_role()), false, 1),
        );

        assert_eq!(
            receivers
                .iter()
                .map(|receiver| receiver.receiver_depth)
                .collect::<Vec<_>>(),
            vec![0, usize::MAX, 1, 3]
        );
        assert_eq!(receivers[1].ty, singleton);
        assert_eq!(receivers[2].receiver_role, Some(lambda_role()));
    }
}
