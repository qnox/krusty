//! Reconcile nested capture sources through the checked construction owner of each operand.

use super::receiver_uses::{ReceiverUseSite, ReceiverUseSites};
use super::{
    AnonymousLexicalClassScope, AnonymousObjectCapture, AnonymousObjectCaptureSource,
    ResolvedCtorDelegation,
};
use crate::ast::{DeclId, ExprId, File};
use crate::fx_hash::{HashMap, HashSet};

pub(super) struct CaptureFieldRemap<'a> {
    pub(super) owner: crate::types::TypeName,
    pub(super) fields: &'a [Option<u32>],
    pub(super) original: &'a [AnonymousObjectCapture],
}

pub(super) fn remap_owned_anonymous_class_storage_captures(
    file: &File,
    owner: DeclId,
    lexical_scope: &AnonymousLexicalClassScope,
    checked_forwards: &HashMap<DeclId, Vec<ExprId>>,
    delegations: &HashMap<DeclId, ResolvedCtorDelegation>,
    remap: CaptureFieldRemap<'_>,
    captures: &mut HashMap<DeclId, Vec<AnonymousObjectCapture>>,
) -> bool {
    let direct_children = |owner| {
        lexical_scope
            .owners
            .iter()
            .filter_map(|(&declaration, &parent)| (parent == owner).then_some(declaration))
            .collect::<Vec<_>>()
    };
    let forwarded_children = |owner| {
        let sites = ReceiverUseSites::anonymous_forwards(
            file,
            owner,
            checked_forwards
                .get(&owner)
                .map(Vec::as_slice)
                .unwrap_or_default(),
            delegations.get(&owner),
        );
        direct_children(owner)
            .into_iter()
            .filter(|declaration| sites.contains(ReceiverUseSite::Declaration(*declaration)))
            .collect::<HashSet<_>>()
    };
    // A forwarded operand executes beside the enclosing construction, not in its syntactic
    // classifier. Its selected operand must be rebased through that construction's original
    // capture source. Other operands keep the finalized owner's field, including those forwarded
    // through several anonymous constructors. Follow only their checked forwarding edges.
    let outside_owner = forwarded_children(owner);
    let mut pending = direct_children(owner)
        .into_iter()
        .map(|child| (child, outside_owner.contains(&child)))
        .collect::<Vec<_>>();
    let mut selected = Vec::new();
    let mut seen = HashSet::default();
    while let Some((child, outside_owner)) = pending.pop() {
        if child == owner || !seen.insert(child) {
            return false;
        }
        pending.extend(
            forwarded_children(child)
                .into_iter()
                .map(|child| (child, outside_owner)),
        );
        selected.push((child, outside_owner));
    }
    remap_selected_sources(selected, remap, captures)
}

fn original_field(
    remap: &CaptureFieldRemap<'_>,
    capture: &AnonymousObjectCapture,
    identity: crate::fir::ClassCaptureIdentity,
) -> Option<usize> {
    let mut fields = remap.original.iter().enumerate().filter(|(_, original)| {
        original.represents_identity(remap.owner, identity)
            && capture.receiver_role == original.receiver_role
    });
    let (field, _) = fields.next()?;
    fields.next().is_none().then_some(field)
}

fn remap_selected_sources(
    selected: Vec<(DeclId, bool)>,
    remap: CaptureFieldRemap<'_>,
    captures: &mut HashMap<DeclId, Vec<AnonymousObjectCapture>>,
) -> bool {
    // The original logical value, not a provisional ordinal, proves every coordinate change.
    for &(declaration, outside_owner) in &selected {
        let Some(captures) = captures.get(&declaration) else {
            continue;
        };
        for capture in captures {
            let AnonymousObjectCaptureSource::ClassStorage(storage) = capture.source else {
                continue;
            };
            if storage.owner != remap.owner {
                continue;
            }
            let Some(field) = original_field(&remap, capture, storage.identity) else {
                return false;
            };
            if !outside_owner && !remap.fields.get(field).is_some_and(Option::is_some) {
                return false;
            }
        }
    }
    for (declaration, outside_owner) in selected {
        if !outside_owner {
            continue;
        }
        let Some(captures) = captures.get_mut(&declaration) else {
            continue;
        };
        for capture in captures {
            let AnonymousObjectCaptureSource::ClassStorage(storage) = capture.source else {
                continue;
            };
            if storage.owner != remap.owner {
                continue;
            }
            let field = original_field(&remap, capture, storage.identity)
                .expect("validated unique original logical capture");
            let original = &remap.original[field];
            capture.source = original.source;
            capture.lexical_binding = original.lexical_binding;
            capture.lexical_shadow_depth = original.lexical_shadow_depth;
            capture.capture_dependency = match original.source {
                AnonymousObjectCaptureSource::ClassStorage(_) => original.capture_dependency,
                _ => Some(storage.identity),
            };
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolve::ClassStorageSource;
    use crate::types::{type_name, Ty};

    fn selected_value(source: ClassStorageSource) -> AnonymousObjectCapture {
        AnonymousObjectCapture {
            name: "value".into(),
            ty: Ty::String,
            shared_cell: false,
            storage_ty: None,
            source: AnonymousObjectCaptureSource::ClassStorage(source),
            receiver_label: None,
            receiver: None,
            semantic_receiver: None,
            lexical_shadow_depth: 0,
            lexical_binding: Some(7),
            capture_dependency: None,
            constructor_projections: Vec::new(),
            receiver_capture: None,
            receiver_role: None,
        }
    }

    #[test]
    fn a_revisited_forward_keeps_the_original_storage_owner_after_its_prefix_retires() {
        let middle = type_name("Middle");
        let outer = ClassStorageSource {
            owner: type_name("Outer"),
            identity: crate::fir::ClassCaptureIdentity::Value {
                owner: type_name("Outer"),
                binding: 7,
            },
        };
        let child = DeclId(12);
        let mut original = selected_value(outer);
        original.lexical_shadow_depth = 1;
        let mut captures = HashMap::default();
        captures.insert(
            child,
            vec![selected_value(ClassStorageSource {
                owner: middle,
                identity: outer.identity,
            })],
        );
        assert!(remap_selected_sources(
            vec![(child, true)],
            CaptureFieldRemap {
                owner: middle,
                fields: &[None],
                original: std::slice::from_ref(&original)
            },
            &mut captures,
        ));
        // The middle's next visit no longer has the discarded prefix. The already-rebased field
        // belongs to Outer, so it must never be indexed through Middle's empty capture inventory.
        assert!(remap_selected_sources(
            vec![(child, true)],
            CaptureFieldRemap {
                owner: middle,
                fields: &[],
                original: &[]
            },
            &mut captures,
        ));
        let [capture] = captures[&child].as_slice() else {
            panic!("the forwarded construction retains its one selected operand")
        };
        assert_eq!(
            capture.source,
            AnonymousObjectCaptureSource::ClassStorage(outer)
        );
        assert_eq!(capture.lexical_binding, original.lexical_binding);
        assert_eq!(capture.lexical_shadow_depth, original.lexical_shadow_depth);
        assert_eq!(capture.capture_dependency, original.capture_dependency);
    }

    #[test]
    fn logical_values_survive_prefix_reorder_and_distinguish_shadowed_checker_bindings() {
        let owner = type_name("LogicalCaptureOwner");
        let first = crate::fir::ClassCaptureIdentity::Value { owner, binding: 7 };
        let second = crate::fir::ClassCaptureIdentity::Value { owner, binding: 8 };
        let other_owner = crate::fir::ClassCaptureIdentity::Value {
            owner: type_name("OtherCaptureOwner"),
            binding: 7,
        };
        let a = selected_value(ClassStorageSource {
            owner,
            identity: first,
        });
        let b = selected_value(ClassStorageSource {
            owner,
            identity: second,
        });
        let (selected, remap) = super::super::capture_field_order::reconcile(
            Some(&[a.clone(), b.clone()]),
            vec![b, a],
            false,
        );
        assert_eq!(remap, [Some(1), Some(0)]);
        assert_eq!(selected[0].logical_identity(owner), Some(second));
        assert_eq!(selected[1].logical_identity(owner), Some(first));
        assert_ne!(first, other_owner);
        let recaptured = selected_value(ClassStorageSource {
            owner: type_name("Recapturer"),
            identity: first,
        });
        assert_eq!(
            recaptured.logical_identity(type_name("LaterRecapturer")),
            Some(first)
        );
    }

    #[test]
    fn a_new_constructor_demand_does_not_retag_an_established_body_capture() {
        let owner = type_name("CanonicalBodyCapture");
        let original = selected_value(ClassStorageSource {
            owner,
            identity: crate::fir::ClassCaptureIdentity::Value { owner, binding: 7 },
        });
        let mut revisit = original.clone();
        revisit.capture_dependency = Some(crate::fir::ClassCaptureIdentity::Value {
            owner: type_name("RequestedConstructorCapture"),
            binding: 7,
        });
        let (selected, fields) = super::super::capture_field_order::reconcile(
            Some(std::slice::from_ref(&original)),
            vec![revisit],
            false,
        );
        let [selected] = selected.as_slice() else {
            panic!("one canonical physical capture")
        };
        assert_eq!(fields, [Some(0)]);
        assert_eq!(
            selected.logical_identity(owner),
            original.logical_identity(owner)
        );
        assert_eq!(selected.capture_dependency, original.capture_dependency);
    }

    #[test]
    fn an_enclosing_class_capture_uses_the_recorded_receiver_role() {
        let owner = type_name("LogicalCaptureOwner");
        let selected = type_name("ActualEnclosingDeclaration");
        let mut capture = selected_value(ClassStorageSource {
            owner,
            identity: crate::fir::ClassCaptureIdentity::Value { owner, binding: 7 },
        });
        capture.source = AnonymousObjectCaptureSource::EnclosingInstance {
            current: true,
            depth: 0,
        };
        capture.lexical_binding = None;
        capture.receiver_role = Some(super::super::ReceiverDeclarationRole::EnclosingClass(
            selected,
        ));
        assert_eq!(
            capture.logical_identity(owner),
            Some(crate::fir::ClassCaptureIdentity::EnclosingClass(selected))
        );
        capture.receiver_role = None;
        assert_eq!(capture.logical_identity(owner), None);
    }

    #[test]
    fn a_required_transport_family_keeps_the_selected_operand_family_distinct() {
        let required_owner = type_name("RequiredCaptureFamily");
        let actual_owner = type_name("ActualStorageOwner");
        let mut required = selected_value(ClassStorageSource {
            owner: actual_owner,
            identity: crate::fir::ClassCaptureIdentity::Value {
                owner: actual_owner,
                binding: 7,
            },
        });
        required.source = AnonymousObjectCaptureSource::LexicalValue;
        let wanted = required
            .logical_identity(required_owner)
            .expect("required lexical capture");
        let actual = ClassStorageSource {
            owner: actual_owner,
            identity: crate::fir::ClassCaptureIdentity::Value {
                owner: actual_owner,
                binding: 7,
            },
        };
        let projected = required
            .forwarded_from(
                required_owner,
                AnonymousObjectCaptureSource::ClassStorage(actual),
            )
            .expect("selected forward mapping");
        assert_ne!(wanted, actual.identity);
        assert_eq!(projected.capture_dependency, Some(wanted));
        assert_eq!(
            projected.logical_identity(type_name("ReceivingClass")),
            Some(wanted)
        );
        assert_eq!(
            projected.source_identity(type_name("ReceivingClass")),
            Some(actual.identity)
        );
        assert_eq!(
            projected.source,
            AnonymousObjectCaptureSource::ClassStorage(actual)
        );
    }
}
