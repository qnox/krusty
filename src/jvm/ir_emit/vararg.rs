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
    // An element whose own emission records a stack-map frame — a branchy inlined body such as
    // `takeUnless { … }` — cannot run with `[array, index]` already on the stack: the relocated
    // frames of a spliced branchy body carry no stack prefix, so the splicer declines and a
    // required stdlib inline body then bails the whole file. Evaluate such elements into temps
    // first, on a clean stack, exactly as the constructor and `Ref` holder paths already do for
    // their own held pairs. Element evaluation stays left-to-right, and a vararg whose elements
    // are all ordinary keeps its existing emission byte for byte.
    if elements
        .iter()
        .any(|&element| emitter.records_frame(element))
    {
        emit_packed_array_through_temps(emitter, array_type, elements, code);
        return;
    }
    code.push_int(elements.len() as i32, emitter.cw);
    if element_type.is_jvm_scalar() && !reference_array {
        code.newarray(prim_newarray_atype(element_type));
    } else {
        // Nullability does not change the reference array class.
        let class = emitter
            .cw
            .class_ref(&crate::jvm::names::instanceof_internal_name(
                element_type.non_null(),
            ));
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

/// Build the same packed array with every element evaluated into a temp first. Used when any element
/// records a frame; see the caller for why the stack must be clean at that point.
fn emit_packed_array_through_temps(
    emitter: &mut Emitter<'_>,
    array_type: &Ty,
    elements: &[u32],
    code: &mut CodeBuilder,
) {
    let element_type = array_jvm_element(array_type);
    let reference_array = array_type.is_reference_array();
    let temps = emitter.spill_to_temps(elements, code);

    code.push_int(elements.len() as i32, emitter.cw);
    if element_type.is_jvm_scalar() && !reference_array {
        code.newarray(prim_newarray_atype(element_type));
    } else {
        let class = emitter.cw.class_ref(&ref_internal(element_type.non_null()));
        code.anewarray(class);
    }

    let jvm_array_type = ir_ty_to_jvm(array_type);
    let slot = emitter.next_slot;
    let words = slot_words(jvm_array_type);
    emitter.next_slot += words;
    store(jvm_array_type, slot, code);
    let slot_key = 2_000_000 + u32::from(slot);
    emitter.slots.insert(slot_key, (slot, jvm_array_type));

    let (store_op, width) = array_store_op(element_type, reference_array);
    let box_element = reference_array
        .then(|| reference_array_scalar_adapter(element_type))
        .flatten();
    for (index, &(temp_slot, temp_ty, _)) in temps.iter().enumerate() {
        load(jvm_array_type, slot, code);
        code.push_int(index as i32, emitter.cw);
        load(temp_ty, temp_slot, code);
        if let Some(primitive) = box_element {
            box_prim_free(emitter.cw, code, primitive);
        }
        code.array_store(store_op, width);
    }
    for &(_, _, key) in &temps {
        emitter.slots.remove(&key);
    }

    load(jvm_array_type, slot, code);
    emitter.slots.remove(&slot_key);
    if emitter.next_slot == slot + words {
        emitter.next_slot = slot;
    }
}
