//! Reconcile nested capture sources through the checked construction owner of each operand.

use super::receiver_uses::{ReceiverUseSite, ReceiverUseSites};
use super::{
    AnonymousLexicalClassScope, AnonymousObjectCapture, AnonymousObjectCaptureSource,
    ResolvedCtorDelegation,
};
use crate::ast::{DeclId, ExprId, File};
use crate::fx_hash::{HashMap, HashSet};

pub(super) fn remap_owned_anonymous_class_storage_captures(
    file: &File,
    owner: DeclId,
    lexical_scope: &AnonymousLexicalClassScope,
    checked_forwards: &HashMap<DeclId, Vec<ExprId>>,
    delegations: &HashMap<DeclId, ResolvedCtorDelegation>,
    fields: &[Option<u32>],
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
    // classifier. Follow only the checked forwarding edges, applying the actual owner's field
    // permutation once even when several anonymous constructors intervene.
    let outside_owner = forwarded_children(owner);
    let mut pending = direct_children(owner)
        .into_iter()
        .filter(|child| !outside_owner.contains(child))
        .collect::<Vec<_>>();
    let mut owned = Vec::new();
    let mut seen = HashSet::default();
    while let Some(child) = pending.pop() {
        if child == owner || !seen.insert(child) {
            return false;
        }
        pending.extend(forwarded_children(child));
        owned.push(child);
    }
    // Validate the complete handoff before changing any descendant's selected storage source.
    if !owned
        .iter()
        .filter_map(|declaration| captures.get(declaration))
        .flatten()
        .all(|capture| match capture.source {
            AnonymousObjectCaptureSource::ClassStorage { field } => {
                fields.get(field as usize).is_some_and(Option::is_some)
            }
            _ => true,
        })
    {
        return false;
    }
    for declaration in owned {
        let Some(captures) = captures.get_mut(&declaration) else {
            continue;
        };
        for capture in captures {
            if let AnonymousObjectCaptureSource::ClassStorage { field } = &mut capture.source {
                *field = fields[*field as usize].expect("validated owner storage permutation");
            }
        }
    }
    true
}
