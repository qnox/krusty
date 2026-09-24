//! JVM realization of a packed vararg/array-literal value.

use super::*;

/// Emit one checked vararg value. Spread validation and both physical array-building strategies
/// live here so frame-recording elements cannot accidentally be handled by only one call shape.
pub(super) fn emit(
    emitter: &mut Emitter<'_>,
    array_type: &Ty,
    elements: &[u32],
    spreads: &[bool],
    code: &mut CodeBuilder,
) {
    if spreads.len() != elements.len() {
        emitter
            .run
            .set_emit_error("vararg spread flags do not match the element list".to_string());
        return;
    }
    if !spreads.iter().any(|&spread| spread) {
        emit_packed_array(emitter, array_type, elements, code);
        return;
    }

    // A spread builder normally remains on the operand stack while each element is evaluated.
    // A frame-recording element cannot inherit that hidden prefix, so evaluate every element on an
    // empty stack first. Spilling all of them preserves source order and exactly-once evaluation.
    let temps = elements
        .iter()
        .any(|&element| emitter.emits_control_flow(element))
        .then(|| emitter.spill_to_temps(elements, code));
    let element_type = array_jvm_element(array_type);
    if element_type.is_jvm_scalar() {
        emit_primitive_spread(
            emitter,
            element_type,
            elements,
            spreads,
            temps.as_deref(),
            code,
        );
    } else {
        emit_reference_spread(
            emitter,
            array_type,
            element_type,
            elements,
            spreads,
            temps.as_deref(),
            code,
        );
    }
    if let Some(temps) = temps {
        for (_, _, lease) in temps {
            emitter.release_temporary(lease);
        }
    }
}

fn emit_primitive_spread(
    emitter: &mut Emitter<'_>,
    element_type: Ty,
    elements: &[u32],
    spreads: &[bool],
    temps: Option<&[(u16, Ty, super::backend_temporaries::TemporaryLease)]>,
    code: &mut CodeBuilder,
) {
    let Some((builder, add_desc, array_desc)) = primitive_spread_builder(element_type) else {
        emitter
            .run
            .set_emit_error("primitive vararg spread has no platform builder".to_string());
        return;
    };
    let class = emitter.cw.class_ref(builder);
    code.new_obj(class);
    code.dup();
    code.push_int(elements.len() as i32, emitter.cw);
    let init = emitter.cw.methodref(builder, "<init>", "(I)V");
    code.invokespecial(init, 1, 0);
    for (index, &element) in elements.iter().enumerate() {
        code.dup();
        if let Some(temps) = temps {
            let (slot, ty, _) = temps[index];
            load(ty, slot, code);
        } else {
            // Preserve the established byte sequence when no child introduces control flow.
            emitter.emit_value(element, code);
        }
        if spreads[index] {
            let add_spread = emitter.cw.methodref(
                "kotlin/jvm/internal/PrimitiveSpreadBuilder",
                "addSpread",
                "(Ljava/lang/Object;)V",
            );
            code.invokevirtual(add_spread, 1, 0);
        } else {
            let add = emitter.cw.methodref(builder, "add", add_desc);
            code.invokevirtual(add, slot_words(element_type) as i32, 0);
        }
    }
    let to_array = emitter
        .cw
        .methodref(builder, "toArray", &format!("(){array_desc}"));
    code.invokevirtual(to_array, 0, 1);
}

fn emit_reference_spread(
    emitter: &mut Emitter<'_>,
    array_type: &Ty,
    element_type: Ty,
    elements: &[u32],
    spreads: &[bool],
    temps: Option<&[(u16, Ty, super::backend_temporaries::TemporaryLease)]>,
    code: &mut CodeBuilder,
) {
    let builder = "kotlin/jvm/internal/SpreadBuilder";
    let class = emitter.cw.class_ref(builder);
    code.new_obj(class);
    code.dup();
    code.push_int(elements.len() as i32, emitter.cw);
    let init = emitter.cw.methodref(builder, "<init>", "(I)V");
    code.invokespecial(init, 1, 0);
    let box_element = reference_array_scalar_adapter(element_type);
    for (index, &element) in elements.iter().enumerate() {
        code.dup();
        if let Some(temps) = temps {
            let (slot, ty, _) = temps[index];
            load(ty, slot, code);
        } else {
            // Preserve the established byte sequence when no child introduces control flow.
            emitter.emit_value(element, code);
        }
        let method = if spreads[index] {
            emitter
                .cw
                .methodref(builder, "addSpread", "(Ljava/lang/Object;)V")
        } else {
            if let Some(primitive) = box_element {
                box_prim_free(emitter.cw, code, primitive);
            }
            emitter
                .cw
                .methodref(builder, "add", "(Ljava/lang/Object;)V")
        };
        code.invokevirtual(method, 1, 0);
    }
    code.push_int(0, emitter.cw);
    let element_class = emitter
        .cw
        .class_ref(&crate::jvm::names::instanceof_internal_name(
            element_type.non_null(),
        ));
    code.anewarray(element_class);
    let to_array = emitter.cw.methodref(
        builder,
        "toArray",
        "([Ljava/lang/Object;)[Ljava/lang/Object;",
    );
    code.invokevirtual(to_array, 1, 1);
    let array_class = emitter
        .cw
        .class_ref(&type_descriptor(ir_ty_to_jvm(array_type)));
    code.checkcast(array_class);
}

/// Build a packed array through the next local slot, matching kotlinc's evaluation and frame shape.
fn emit_packed_array(
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
        .any(|&element| emitter.emits_control_flow(element))
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
    let array_lease = emitter.lease_temporary(slot, array_type);

    let (store_op, width) = array_store_op(element_type, reference_array);
    let box_element = reference_array
        .then(|| reference_array_scalar_adapter(element_type))
        .flatten();
    for (index, &element) in elements.iter().enumerate() {
        load(array_type, slot, code);
        code.push_int(index as i32, emitter.cw);
        emitter.emit_value(element, code);
        if let Some(primitive) = box_element {
            box_prim_free(emitter.cw, code, primitive);
        }
        code.array_store(store_op, width);
    }

    load(array_type, slot, code);
    emitter.release_temporary(array_lease);
    if emitter.next_slot == slot + words {
        emitter.next_slot = slot;
    }
}

/// Build the same packed array with every element evaluated into a temp first. Used when any element
/// introduces control flow; see the caller for why the stack must be clean at that point.
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
        let class = emitter
            .cw
            .class_ref(&crate::jvm::names::instanceof_internal_name(
                element_type.non_null(),
            ));
        code.anewarray(class);
    }

    let jvm_array_type = ir_ty_to_jvm(array_type);
    let slot = emitter.next_slot;
    let words = slot_words(jvm_array_type);
    emitter.next_slot += words;
    store(jvm_array_type, slot, code);
    let array_lease = emitter.lease_temporary(slot, jvm_array_type);

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
    for &(_, _, lease) in &temps {
        emitter.release_temporary(lease);
    }

    load(jvm_array_type, slot, code);
    emitter.release_temporary(array_lease);
    if emitter.next_slot == slot + words {
        emitter.next_slot = slot;
    }
}
