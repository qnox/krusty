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

/// The interfaces a class implements itself, and those reached only through its superclasses.
///
/// A subclass's most specific default includes members inherited through the class hierarchy.
/// `own` stays in source order; `inherited` lists each superclass's direct interfaces, nearest
/// superclass first.
pub(super) struct ImplementedInterfaces {
    pub own: Vec<crate::types::TypeName>,
    pub inherited: Vec<crate::types::TypeName>,
}

pub(super) fn implemented_interfaces(
    ir: &crate::ir::IrFile,
    symbols: &dyn BackendClassifierSource,
    class: &crate::ir::IrClass,
) -> ImplementedInterfaces {
    let own = class.interfaces.iter_ids().collect();
    let mut inherited = Vec::new();
    let mut next = superclass_name(class.superclass);
    let mut seen = std::collections::HashSet::new();
    while let Some(current) = next {
        if !seen.insert(current) {
            break;
        }
        if let Some(super_class) = ir
            .classes
            .iter()
            .find(|candidate| candidate.fq_name_id() == current)
        {
            inherited.extend(super_class.interfaces.iter_ids());
            next = superclass_name(super_class.superclass);
            continue;
        }
        let Some(shape) = symbols.classifier(current) else {
            break;
        };
        for supertype in shape.supertypes.iter().copied() {
            if symbols
                .classifier(supertype)
                .is_some_and(|parent| parent.is_interface())
            {
                inherited.push(supertype);
            }
        }
        next = shape.supertypes.iter().copied().find(|supertype| {
            !is_root_class(*supertype)
                && symbols
                    .classifier(*supertype)
                    .is_some_and(|parent| !parent.is_interface())
        });
    }
    ImplementedInterfaces { own, inherited }
}

fn superclass_name(superclass: crate::types::TypeName) -> Option<crate::types::TypeName> {
    (!is_root_class(superclass)).then_some(superclass)
}

fn is_root_class(name: crate::types::TypeName) -> bool {
    name == crate::types::type_name("java/lang/Object")
        || name == crate::types::type_name("kotlin/Any")
}
