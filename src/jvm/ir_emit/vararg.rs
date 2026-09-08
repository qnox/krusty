//! JVM realization of a packed vararg/array-literal value.

use super::*;

/// Build a packed array through the next local slot, matching kotlinc's evaluation and frame shape.
pub(super) fn emit_packed_array(
    emitter: &mut Emitter<'_>,
    array_type: &Ty,
    elements: &[u32],
    code: &mut CodeBuilder,
) {
    let element_type = array_jvm_element(array_type);
    let reference_array = array_type.is_reference_array();
    code.push_int(elements.len() as i32, emitter.cw);
    if element_type.is_jvm_scalar() && !reference_array {
        code.newarray(prim_newarray_atype(element_type));
    } else {
        // Nullability does not change the reference array class.
        let class = emitter.cw.class_ref(&ref_internal(element_type.non_null()));
        code.anewarray(class);
    }

    let array_type = ir_ty_to_jvm(array_type);
    let slot = emitter.next_slot;
    let words = slot_words(array_type);
    emitter.next_slot += words;
    store(array_type, slot, code);
    let slot_key = 2_000_000 + u32::from(slot);
    emitter.slots.insert(slot_key, (slot, array_type));

    let held = [emitter.verif_single(array_type), VerifType::Integer];
    let (store_op, width) = array_store_op(element_type, reference_array);
    let box_element = reference_array
        .then(|| reference_array_scalar_adapter(element_type))
        .flatten();
    for (index, &element) in elements.iter().enumerate() {
        load(array_type, slot, code);
        code.push_int(index as i32, emitter.cw);
        emitter.emit_value_over(element, &held, code);
        if let Some(primitive) = box_element {
            box_prim_free(emitter.cw, code, primitive);
        }
        code.array_store(store_op, width);
    }

    load(array_type, slot, code);
    emitter.slots.remove(&slot_key);
    if emitter.next_slot == slot + words {
        emitter.next_slot = slot;
    }
}
