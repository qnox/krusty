//! Companion initialization is not yet part of this target's ordinary construction protocol.
//! Unconstructed holders need no such protocol: their companion is an existing lazy singleton.

use krusty::fx_hash::HashSet;
use krusty::ir::{ClassId, IrExpr, IrFile};

/// Whether an emitted construction can run this holder's constructor, directly or as a superclass.
/// Default/export entries and constructor-reference adapters already expose their `New` bodies
/// before class layout. Singleton getters allocate outside those bodies, so their constructors
/// participate explicitly. Their declaring outer class is not an instance-construction edge.
pub(super) fn requires_initialization(ir: &IrFile, holder: ClassId) -> bool {
    let mut pending = krusty::ir::emitted_body_roots(ir)
        .into_iter()
        .flat_map(|root| krusty::ir::value_namespace_expressions(ir, root))
        .filter_map(|expression| match ir.expr(expression) {
            IrExpr::New { internal, .. } => ir.class_id_by_name(*internal),
            _ => None,
        })
        .chain(
            ir.classes
                .iter()
                .enumerate()
                .filter_map(|(class, declaration)| {
                    (declaration.is_object || declaration.is_companion).then_some(class as ClassId)
                }),
        )
        .collect::<Vec<_>>();
    let mut seen = HashSet::default();
    while let Some(class) = pending.pop() {
        if class == holder {
            return true;
        }
        if !seen.insert(class) {
            continue;
        }
        // The same exact superclass relation supplies the backend's constructor table.
        if let Some(parent) = ir
            .classes
            .get(class as usize)
            .and_then(|class| ir.class_id_by_name(class.superclass))
        {
            pending.push(parent);
        }
    }
    false
}

#[cfg(test)]
mod tests;
