//! Checked interface hierarchy operations shared by compatibility forwarder emitters.

use crate::backend::BackendClassifierSource;

/// Whether `candidate` transitively derives from the interface `ancestor`, read through the
/// normalized classifier model.
pub(super) fn derives_from(
    symbols: &dyn BackendClassifierSource,
    candidate: crate::types::TypeName,
    ancestor: crate::types::TypeName,
) -> bool {
    let mut pending = vec![candidate];
    let mut seen = std::collections::HashSet::new();
    while let Some(owner) = pending.pop() {
        if !seen.insert(owner) {
            continue;
        }
        let Some(shape) = symbols.classifier(owner) else {
            continue;
        };
        for parent in shape.supertypes.iter().copied() {
            if parent == ancestor {
                return true;
            }
            if symbols
                .classifier(parent)
                .is_some_and(|parent| parent.is_interface())
            {
                pending.push(parent);
            }
        }
    }
    false
}

/// The transitive interface closure of the `direct` supertypes in topological order: every
/// interface precedes all of its ancestors, while incomparable interfaces retain declaration
/// order.
pub(super) fn sorted_closure(
    symbols: &dyn BackendClassifierSource,
    direct: Vec<crate::types::TypeName>,
) -> Vec<(
    crate::types::TypeName,
    std::sync::Arc<crate::backend::BackendClassifierFact>,
)> {
    fn visit(
        symbols: &dyn BackendClassifierSource,
        owner: crate::types::TypeName,
        seen: &mut std::collections::HashSet<crate::types::TypeName>,
        out: &mut Vec<(
            crate::types::TypeName,
            std::sync::Arc<crate::backend::BackendClassifierFact>,
        )>,
    ) {
        if !seen.insert(owner) {
            return;
        }
        let Some(shape) = symbols
            .classifier(owner)
            .filter(|shape| shape.is_interface())
        else {
            return;
        };
        for parent in shape.supertypes.iter().rev().copied() {
            visit(symbols, parent, seen, out);
        }
        out.push((owner, shape));
    }

    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for owner in direct.into_iter().rev() {
        visit(symbols, owner, &mut seen, &mut out);
    }
    out.reverse();
    out
}
