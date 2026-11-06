//! Reconcile nested capture sources through the checked construction owner of each operand.

use super::receiver_uses::{ReceiverUseSite, ReceiverUseSites};
use super::{
    AnonymousLexicalClassScope, AnonymousObjectCapture, AnonymousObjectCaptureSource,
    ResolvedCtorDelegation,
};
use crate::ast::{DeclId, ExprId, File};
use crate::fx_hash::{HashMap, HashSet};

pub(super) struct CaptureFieldRemap<'a> {
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
    // Validate both coordinate changes before changing any descendant's selected source.
    for &(declaration, outside_owner) in &selected {
        let Some(captures) = captures.get(&declaration) else {
            continue;
        };
        for capture in captures {
            let AnonymousObjectCaptureSource::ClassStorage { field } = capture.source else {
                continue;
            };
            let valid = if outside_owner {
                remap
                    .original
                    .get(field as usize)
                    .is_some_and(|original| capture.receiver_role == original.receiver_role)
            } else {
                remap
                    .fields
                    .get(field as usize)
                    .is_some_and(Option::is_some)
            };
            if !valid {
                return false;
            }
        }
    }
    for (declaration, outside_owner) in selected {
        let Some(captures) = captures.get_mut(&declaration) else {
            continue;
        };
        for capture in captures {
            let AnonymousObjectCaptureSource::ClassStorage { field } = capture.source else {
                continue;
            };
            if outside_owner {
                let original = &remap.original[field as usize];
                capture.source = original.source;
                capture.lexical_binding = original.lexical_binding;
                capture.lexical_shadow_depth = original.lexical_shadow_depth;
                capture.capture_dependency = original.capture_dependency;
            } else {
                capture.source = AnonymousObjectCaptureSource::ClassStorage {
                    field: remap.fields[field as usize]
                        .expect("validated owner storage permutation"),
                };
            }
        }
    }
    true
}
