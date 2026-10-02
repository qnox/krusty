//! JVM realization of a packed vararg/array-literal value.

use super::frame_map::TempRole;
use super::*;
use crate::jvm::array_representation::array_store_op;

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

    let element_type = array_jvm_element(array_type);
    // A sole spread of an array built for this argument is that array. Every other sole primitive
    // spread is `Arrays.copyOf(array, array.length)`, so the caller's array is not aliased.
    if elements.len() == 1
        && spreads[0]
        && element_type.is_jvm_scalar()
        && matches!(
            emitter.ir.expr(elements[0]),
            IrExpr::Vararg { .. } | IrExpr::NewArray { .. }
        )
    {
        emitter.emit_value(elements[0], code);
        return;
    }

    // A spread builder is stored and reloaded around each element. An element that cannot carry
    // the reloaded builder (see `spills_operand_prefix`) is evaluated with every other element on
    // an empty stack first, preserving source order and exactly-once evaluation.
    let temps = elements
        .iter()
        .any(|&element| emitter.spills_operand_prefix(element))
        .then(|| emitter.spill_to_temps(elements, code));
    if elements.len() == 1 && spreads[0] && element_type.is_jvm_scalar() {
        emit_primitive_copy(emitter, element_type, elements[0], temps.as_deref(), code);
    } else if element_type.is_jvm_scalar() {
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
        emitter.release_operand_spills(&temps);
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
    // kotlinc parks the builder in a local and reloads it for every `add` / `addSpread` / `toArray`.
    let builder_ty = Ty::obj(builder);
    let held = emitter.frame.enter_temp(TempRole::VarargArray, builder_ty);
    let builder_slot = held.slot();
    store(builder_ty, builder_slot, code);
    let builder_lease = emitter.lease_frame_temporary(held, builder_ty);
    for (index, &element) in elements.iter().enumerate() {
        load(builder_ty, builder_slot, code);
        if let Some(temps) = temps {
            let (slot, ty, _) = temps[index];
            load(ty, slot, code);
        } else {
            emitter.emit_value(element, code);
        }
        if spreads[index] {
            let add_spread = emitter
                .cw
                .methodref(builder, "addSpread", "(Ljava/lang/Object;)V");
            code.invokevirtual(add_spread, 1, 0);
        } else {
            let add = emitter.cw.methodref(builder, "add", add_desc);
            code.invokevirtual(add, slot_words(element_type) as i32, 0);
        }
    }
    load(builder_ty, builder_slot, code);
    let to_array = emitter
        .cw
        .methodref(builder, "toArray", &format!("(){array_desc}"));
    code.invokevirtual(to_array, 0, 1);
    emitter.release_temporary(builder_lease);
}

/// `Arrays.copyOf(array, array.length)` for one existing primitive array. A local is loaded twice;
/// any other producer is stored once and then loaded twice.
fn emit_primitive_copy(
    emitter: &mut Emitter<'_>,
    element_type: Ty,
    element: u32,
    temps: Option<&[(u16, Ty, super::backend_temporaries::TemporaryLease)]>,
    code: &mut CodeBuilder,
) {
    let Some((_, _, array_desc)) = primitive_spread_builder(element_type) else {
        emitter
            .run
            .set_emit_error("primitive vararg spread has no platform copy".to_string());
        return;
    };
    place_array_and_length(emitter, element, temps, code);
    let copy = emitter.cw.methodref(
        "java/util/Arrays",
        "copyOf",
        &format!("({array_desc}I){array_desc}"),
    );
    code.invokestatic(copy, 2, 1);
}

fn place_array_and_length(
    emitter: &mut Emitter<'_>,
    element: u32,
    temps: Option<&[(u16, Ty, super::backend_temporaries::TemporaryLease)]>,
    code: &mut CodeBuilder,
) {
    if let Some(temps) = temps {
        let (slot, ty, _) = temps[0];
        load(ty, slot, code);
        load(ty, slot, code);
        code.arraylength();
        return;
    }
    if matches!(emitter.ir.expr(element), IrExpr::GetValue(_)) {
        emitter.emit_value(element, code);
        emitter.emit_value(element, code);
        code.arraylength();
        return;
    }
    emitter.emit_value(element, code);
    let ty = emitter.value_ty(element);
    let held = emitter.frame.enter_temp(TempRole::VarargArray, ty);
    let slot = held.slot();
    store(ty, slot, code);
    let lease = emitter.lease_frame_temporary(held, ty);
    load(ty, slot, code);
    load(ty, slot, code);
    code.arraylength();
    emitter.release_temporary(lease);
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
        let produced = temps.map_or_else(|| emitter.value_ty(element), |temps| temps[index].1);
        let method = if spreads[index] {
            emitter
                .cw
                .methodref(builder, "addSpread", "(Ljava/lang/Object;)V")
        } else {
            box_reference_scalar(emitter, box_element, produced, code);
            emitter
                .cw
                .methodref(builder, "add", "(Ljava/lang/Object;)V")
        };
        code.invokevirtual(method, 1, 0);
    }
    code.push_int(0, emitter.cw);
    let element_class = emitter
        .cw
        .class_ref(&crate::jvm::names::anewarray_element_class(element_type));
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
pub(super) fn emit_packed_array(
    emitter: &mut Emitter<'_>,
    array_type: &Ty,
    elements: &[u32],
    code: &mut CodeBuilder,
) {
    let element_type = array_jvm_element(array_type);
    let reference_array = array_type.is_reference_array();
    // An element that cannot carry `[array, index]` on the stack (see `spills_operand_prefix`),
    // such as a branchy inlined `takeUnless { … }`, is evaluated with every other element into
    // temps first, on a clean stack. Element evaluation stays left-to-right; an ordinary branchy
    // element keeps the pair on the stack, as kotlinc does.
    if elements
        .iter()
        .any(|&element| emitter.spills_operand_prefix(element))
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
            .class_ref(&crate::jvm::names::anewarray_element_class(element_type));
        code.anewarray(class);
    }

    let array_type = ir_ty_to_jvm(array_type);
    let array = emitter.frame.enter_temp(TempRole::VarargArray, array_type);
    let slot = array.slot();
    store(array_type, slot, code);
    let array_lease = emitter.lease_frame_temporary(array, array_type);

    let (store_op, width) = array_store_op(element_type, reference_array);
    let box_element = reference_array
        .then(|| reference_array_scalar_adapter(element_type))
        .flatten();
    for (index, &element) in elements.iter().enumerate() {
        load(array_type, slot, code);
        code.push_int(index as i32, emitter.cw);
        emitter.emit_value(element, code);
        // A nullable unsigned element is already the boxed value (or null). Boxing again calls
        // `box-impl` on a reference.
        box_reference_scalar(emitter, box_element, emitter.value_ty(element), code);
        code.array_store(store_op, width);
    }

    load(array_type, slot, code);
    emitter.release_temporary(array_lease);
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
            .class_ref(&crate::jvm::names::anewarray_element_class(element_type));
        code.anewarray(class);
    }

    let jvm_array_type = ir_ty_to_jvm(array_type);
    let array = emitter
        .frame
        .enter_temp(TempRole::VarargArray, jvm_array_type);
    let slot = array.slot();
    store(jvm_array_type, slot, code);
    let array_lease = emitter.lease_frame_temporary(array, jvm_array_type);

    let (store_op, width) = array_store_op(element_type, reference_array);
    let box_element = reference_array
        .then(|| reference_array_scalar_adapter(element_type))
        .flatten();
    for (index, &(temp_slot, temp_ty, _)) in temps.iter().enumerate() {
        load(jvm_array_type, slot, code);
        code.push_int(index as i32, emitter.cw);
        load(temp_ty, temp_slot, code);
        box_reference_scalar(emitter, box_element, temp_ty, code);
        code.array_store(store_op, width);
    }

    // Newest first: the array was entered above the element temporaries.
    load(jvm_array_type, slot, code);
    emitter.release_temporary(array_lease);
    emitter.release_operand_spills(&temps);
}

/// Box a reference-array element that is still a JVM scalar. An element that is already the boxed
/// value, including `null`, stays as it is: a second unsigned `box-impl` does not accept a reference.
fn box_reference_scalar(
    emitter: &mut Emitter<'_>,
    primitive: Option<Ty>,
    produced: Ty,
    code: &mut CodeBuilder,
) {
    if let Some(primitive) = primitive.filter(|_| produced.is_jvm_scalar()) {
        box_prim_free(emitter.cw, code, primitive);
    }
}
