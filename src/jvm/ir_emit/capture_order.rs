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
///
/// A prefix value with no constructor argument, a field index past the field table, and two prefix
/// values naming one field are malformed IR.
pub(in crate::jvm) fn physical_fields<T>(ir: &IrFile, class: &IrClass, items: Vec<T>) -> Vec<T> {
    if order(ir, class.fq_name).is_none() {
        return items;
    }
    let prefix = (0..class.constructor_prefix_count).collect();
    let prefix_fields = physical(ir, class, prefix)
        .into_iter()
        .filter_map(|identity| {
            class
                .ctor_args
                .get(identity as usize)
                .unwrap_or_else(|| {
                    panic!(
                        "malformed IR: constructor prefix value {identity} of {} has no argument",
                        class.fq_name()
                    )
                })
                .field_index
        })
        .collect::<Vec<_>>();
    let mut items = items.into_iter().map(Some).collect::<Vec<_>>();
    let mut placed = prefix_fields
        .iter()
        .map(|&field| {
            items
                .get_mut(field as usize)
                .unwrap_or_else(|| {
                    panic!(
                        "malformed IR: constructor prefix field {field} of {} is not a field",
                        class.fq_name()
                    )
                })
                .take()
                .unwrap_or_else(|| {
                    panic!(
                        "malformed IR: two constructor prefix values of {} store field {field}",
                        class.fq_name()
                    )
                })
        })
        .collect::<Vec<_>>();
    placed.extend(items.into_iter().flatten());
    placed
}

#[cfg(test)]
mod tests {
    use super::physical_fields;
    use crate::ir::{IrClass, IrCtorArg, IrCtorParameterProvenance, IrField, IrFile};
    use crate::types::{ContextParameterKind, Ty};

    fn capture(field_index: Option<u32>) -> IrCtorArg {
        IrCtorArg {
            name: None,
            context_kind: ContextParameterKind::None,
            ty: Ty::Int,
            declared_ty: None,
            is_field: field_index.is_some(),
            field_index,
            has_default: false,
            is_vararg: false,
            type_param: None,
            check: None,
            anonymous_super_forward: None,
            capture: None,
            provenance: IrCtorParameterProvenance::Capture,
            capture_identity: None,
        }
    }

    /// A local class with `fields` and the constructor prefix `prefix`, emitted in `order`.
    fn layout(fields: &[&str], prefix: Vec<IrCtorArg>, order: &[u32]) -> (IrFile, u32) {
        let mut ir = IrFile::default();
        let mut class = IrClass::synthetic(crate::types::type_name("Local"));
        class.fields = fields
            .iter()
            .map(|name| IrField::new((*name).to_string(), Ty::Int))
            .collect();
        class.constructor_prefix_count = u32::try_from(order.len()).expect("small fixture");
        class.ctor_args = prefix;
        let class = ir.add_class(class);
        ir.class_capture_order.insert(class, order.into());
        (ir, class)
    }

    fn placed(fields: &[&str], prefix: Vec<IrCtorArg>, order: &[u32]) -> Vec<String> {
        let (ir, class) = layout(fields, prefix, order);
        let names = fields.iter().map(|name| (*name).to_string()).collect();
        physical_fields(&ir, &ir.classes[class as usize], names)
    }

    /// The constructor takes `(c, b, a)`; `c` has no field, so the fields are `$b`, `$a`, then
    /// the class's own field.
    #[test]
    fn a_prefix_value_without_a_field_has_no_entry() {
        assert_eq!(
            placed(
                &["$a", "$b", "own"],
                vec![capture(Some(0)), capture(Some(1)), capture(None)],
                &[2, 1, 0],
            ),
            ["$b", "$a", "own"]
        );
    }

    #[test]
    #[should_panic(expected = "malformed IR: constructor prefix value 1 of Local has no argument")]
    fn a_prefix_value_without_an_argument_is_malformed() {
        placed(&["$a"], vec![capture(Some(0))], &[1, 0]);
    }

    #[test]
    #[should_panic(expected = "malformed IR: constructor prefix field 2 of Local is not a field")]
    fn a_prefix_field_past_the_field_table_is_malformed() {
        placed(
            &["$a", "$b"],
            vec![capture(Some(0)), capture(Some(2))],
            &[1, 0],
        );
    }

    #[test]
    #[should_panic(expected = "malformed IR: two constructor prefix values of Local store field 0")]
    fn two_prefix_values_storing_one_field_are_malformed() {
        placed(
            &["$a", "$b"],
            vec![capture(Some(0)), capture(Some(0))],
            &[1, 0],
        );
    }
}
