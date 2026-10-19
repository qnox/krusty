//! kotlinc's physical order of a local class's captures.
//!
//! Common IR keeps a local class's capture fields, constructor prefix and construction arguments in
//! field order, which is their identity, and records kotlinc's first-use order beside them
//! (`IrFile::class_capture_order`). Field and parameter order is JVM representation, so this module
//! is the one place that maps a capture's identity to its physical position. The field table, the
//! constructor descriptors and their parameter slots, and every construction's arguments go through
//! it; reads keep naming the identity.

use crate::ir::{IrClass, IrFile};
use crate::types::TypeName;

/// `order[position]` is the capture identity (field and constructor-prefix index) the class emits
/// at `position`, or `None` when it emits its captures in field order. A recorded order that is not
/// a permutation of the leading constructor prefix is malformed IR.
fn order(ir: &IrFile, owner: TypeName) -> Option<&[u32]> {
    let class_id = ir.class_id_by_name(owner)?;
    let order = ir.class_capture_order.get(&class_id)?;
    let class = &ir.classes[class_id as usize];
    let mut seen = vec![false; order.len()];
    let permutation = order.len() <= class.constructor_prefix_count as usize
        && order.iter().all(|&identity| {
            seen.get_mut(identity as usize)
                .is_some_and(|seen| !std::mem::replace(seen, true))
        });
    assert!(
        permutation,
        "malformed IR: capture order {order:?} of {} is not a permutation of its constructor prefix",
        class.fq_name()
    );
    Some(order)
}

/// `items`, given in identity order with the constructor prefix first, in physical order.
pub(in crate::jvm) fn physical<T>(ir: &IrFile, class: &IrClass, items: Vec<T>) -> Vec<T> {
    physical_for(ir, class.fq_name, items)
}

/// [`physical`] for the class named `owner`, which a construction site knows only by name.
pub(in crate::jvm) fn physical_for<T>(ir: &IrFile, owner: TypeName, items: Vec<T>) -> Vec<T> {
    let Some(order) = order(ir, owner) else {
        return items;
    };
    assert!(
        items.len() >= order.len(),
        "malformed IR: {} capture positions for {} prefix entries of {}",
        order.len(),
        items.len(),
        owner.render()
    );
    let mut items = items.into_iter().map(Some).collect::<Vec<_>>();
    let mut placed = order
        .iter()
        .map(|&identity| {
            items[identity as usize]
                .take()
                .expect("a permutation names each identity once")
        })
        .collect::<Vec<_>>();
    placed.extend(items.into_iter().skip(order.len()).flatten());
    placed
}

/// The physical position of the constructor value or field whose identity is `identity`.
pub(in crate::jvm) fn position(ir: &IrFile, class: &IrClass, identity: u32) -> u32 {
    order(ir, class.fq_name)
        .and_then(|order| order.iter().position(|&ordered| ordered == identity))
        .map_or(identity, |position| position as u32)
}

/// `items`, given in field-table order, in physical order. The fields that store the constructor
/// prefix follow its physical order; a prefix value with no field (a capture only constructor code
/// reads, a forwarded super-constructor argument) has no entry. Every other field keeps its place
/// after them. Without a recorded order the prefix fields already lead in prefix order.
pub(in crate::jvm) fn physical_fields<T>(ir: &IrFile, class: &IrClass, items: Vec<T>) -> Vec<T> {
    if order(ir, class.fq_name).is_none() {
        return items;
    }
    let prefix = (0..class.constructor_prefix_count).collect();
    let prefix_fields = physical(ir, class, prefix)
        .into_iter()
        .filter_map(|identity| class.ctor_args.get(identity as usize)?.field_index)
        .collect::<Vec<_>>();
    let mut items = items.into_iter().map(Some).collect::<Vec<_>>();
    let mut placed = prefix_fields
        .iter()
        .filter_map(|&field| items.get_mut(field as usize)?.take())
        .collect::<Vec<_>>();
    placed.extend(items.into_iter().flatten());
    placed
}
