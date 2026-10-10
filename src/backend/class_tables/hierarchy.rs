//! The order classes are tabled in, and the interfaces each one implements.

use crate::ir::{ClassId, IrFile};
use crate::types::Ty;

use super::{Representation, Unsupported};

/// Every interface each class implements, transitively: its own, its superclass's, and the bases
/// of both. An interface's entry holds the interfaces it extends, not itself.
pub(crate) fn interface_closure(ir: &IrFile) -> Vec<Vec<ClassId>> {
    let direct = |id: ClassId| -> Vec<ClassId> {
        let class = &ir.classes[id as usize];
        class
            .interfaces
            .iter()
            .chain(
                class
                    .supertypes
                    .iter()
                    .copied()
                    .filter_map(Ty::obj_internal),
            )
            .filter_map(|name| ir.class_id_by_name(name))
            .filter(|&candidate| ir.classes[candidate as usize].is_interface)
            .collect()
    };
    let mut closures: Vec<Vec<ClassId>> = vec![Vec::new(); ir.classes.len()];
    // A fixed point rather than an order-dependent single pass: a class may precede an interface
    // it implements in IR order, and the closure is small enough that iterating to stability costs
    // nothing.
    loop {
        let mut changed = false;
        for id in 0..ir.classes.len() as ClassId {
            let mut collected: Vec<ClassId> = closures[id as usize].clone();
            let add = |collected: &mut Vec<ClassId>, candidate: ClassId| {
                if !collected.contains(&candidate) {
                    collected.push(candidate);
                }
            };
            for candidate in direct(id) {
                add(&mut collected, candidate);
                for inherited in closures[candidate as usize].clone() {
                    add(&mut collected, inherited);
                }
            }
            if let Some(parent) = ir.class_id_by_name(ir.classes[id as usize].superclass) {
                for inherited in closures[parent as usize].clone() {
                    add(&mut collected, inherited);
                }
            }
            if collected.len() != closures[id as usize].len() {
                closures[id as usize] = collected;
                changed = true;
            }
        }
        if !changed {
            return closures;
        }
    }
}

/// Classes sorted so that everything a class is laid out FROM precedes it: its superclass, whose
/// fields and vtable it extends, and — for an interface — the interfaces it extends, whose member
/// numbering it inherits. A superclass that is not in this file is declined, unless it is
/// `kotlin.Any` or a base the target's runtime owns ([`Representation::owns_base`]).
///
/// This is a topological sort rather than a sort by hierarchy depth. Depth is not a valid key
/// here: two classes can sit at the same recorded depth with one extending the other, once
/// interfaces contribute their own depths to the same number, and laying out a subclass before its
/// superclass reads a layout that does not exist yet.
pub(crate) fn hierarchy_order(
    representation: &dyn Representation,
    ir: &IrFile,
) -> Result<Vec<ClassId>, Unsupported> {
    for class in &ir.classes {
        if class.superclass != crate::types::wk::any()
            && !representation.owns_base(class.superclass)
            && ir.class_id_by_name(class.superclass).is_none()
        {
            return Err(format!(
                "a superclass declared outside this file (`{}` extends `{}`)",
                class.fq_name(),
                class.superclass.render()
            ));
        }
    }
    let requires = |id: ClassId| -> Vec<ClassId> {
        let class = &ir.classes[id as usize];
        let mut needed: Vec<ClassId> = ir.class_id_by_name(class.superclass).into_iter().collect();
        if class.is_interface {
            needed.extend(
                class
                    .interfaces
                    .iter()
                    .chain(
                        class
                            .supertypes
                            .iter()
                            .copied()
                            .filter_map(Ty::obj_internal),
                    )
                    .filter_map(|name| ir.class_id_by_name(name)),
            );
        }
        needed.retain(|&needed| needed != id);
        needed
    };
    let mut placed = vec![false; ir.classes.len()];
    let mut order = Vec::with_capacity(ir.classes.len());
    // IR order within a level, so emission stays deterministic for a given source.
    while order.len() < ir.classes.len() {
        let mut progressed = false;
        for id in 0..ir.classes.len() as ClassId {
            if placed[id as usize] || !requires(id).iter().all(|&need| placed[need as usize]) {
                continue;
            }
            placed[id as usize] = true;
            order.push(id);
            progressed = true;
        }
        if !progressed {
            // A cycle among supertypes: not expressible in Kotlin, so this is a defect in the
            // hierarchy the frontend handed over rather than something to lower.
            return Err("a cycle among supertypes".to_string());
        }
    }
    Ok(order)
}
