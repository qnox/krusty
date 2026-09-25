//! The implicit rungs of the unqualified name tower: implicit value receivers interleaved with the
//! static scopes of the classifiers open at a site.
//!
//! Checked bodies and compact signature solving both walk this one priority order, so an
//! unqualified call, property read or `::name` reference binds the same declaration on either
//! path. Each side supplies its own receiver representation and says which classifier's instance
//! a receiver is.

use crate::types::TypeName;

/// One rung of the unqualified tower, nearest first.
pub(super) enum ImplicitRung<R> {
    Receiver(R),
    StaticScope(TypeName),
}

/// Interleave `receivers` (nearest first) with `static_scopes` (innermost first). A classifier's
/// static scope directly follows the receiver that `instance_of` names as that classifier's own
/// instance, ahead of every outer receiver; a static scope with no such receiver (a companion
/// extension's classifier) follows every receiver.
pub(super) fn implicit_rungs<R>(
    receivers: impl IntoIterator<Item = R>,
    mut static_scopes: Vec<TypeName>,
    instance_of: impl Fn(&R) -> Option<TypeName>,
) -> Vec<ImplicitRung<R>> {
    let mut rungs = Vec::new();
    for receiver in receivers {
        let own_scope = instance_of(&receiver)
            .and_then(|classifier| static_scopes.iter().position(|open| *open == classifier))
            .map(|index| static_scopes.remove(index));
        rungs.push(ImplicitRung::Receiver(receiver));
        rungs.extend(own_scope.map(ImplicitRung::StaticScope));
    }
    rungs.extend(static_scopes.into_iter().map(ImplicitRung::StaticScope));
    rungs
}
