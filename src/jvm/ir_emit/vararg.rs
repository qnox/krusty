//! JVM realization of a packed vararg/array-literal value.

use super::frame_map::TempRole;
use super::*;
use crate::jvm::array_representation::array_store_op;

/// An array value to create: its type, and the reification argument kotlinc marks its creation
/// with when a reified body creates it of its own type parameter (`ReifiedMarkers::arrays`).
/// `source` is the vararg expression the array realizes, whose source line owns the array's own
/// instructions; a generated array has none.
#[derive(Clone, Copy)]
pub(super) struct NewArray<'a> {
    pub(super) ty: &'a Ty,
    pub(super) reified_marker: Option<&'a str>,
    pub(super) source: Option<ExprId>,
}

/// Return to the vararg's own line before an instruction of the array itself.
///
/// kotlinc lowers a vararg to `tmp = arrayOfNulls(n); tmp[0] = e0; …; tmp`, every generated
/// statement at the vararg's offsets. Its line therefore comes back before each element store's
/// `[array, index]` prefix and before the final array load, while each element marks its own line.
fn mark_array_line(emitter: &mut Emitter<'_>, array: NewArray<'_>, code: &mut CodeBuilder) {
    if let Some(source) = array.source {
        emitter.mark_expression_start(source, code);
    }
}

/// `anewarray` of `element_type`'s class over the length on the stack, preceded by kotlinc's
/// `reifiedOperationMarker(0, argument)` (`NEW_ARRAY`) when `reified_marker` names one
/// (`ExpressionCodegen.newArrayInstruction`).
pub(super) fn emit_reference_new_array(
    emitter: &mut Emitter<'_>,
    element_type: Ty,
    reified_marker: Option<&str>,
    code: &mut CodeBuilder,
) {
    if let Some(argument) = reified_marker {
        code.push_int(0, emitter.cw);
        code.push_string(argument, emitter.cw);
        let marker = emitter.cw.methodref(
            "kotlin/jvm/internal/Intrinsics",
            "reifiedOperationMarker",
            "(ILjava/lang/String;)V",
        );
        code.invokestatic(marker, 2, 0);
    }
    let class = emitter
        .cw
        .class_ref(&crate::jvm::names::anewarray_element_class(element_type));
    code.anewarray(class);
}

/// Emit one checked vararg value. Spread validation and both physical array-building strategies
/// live here so frame-recording elements cannot accidentally be handled by only one call shape.
pub(super) fn emit(
    emitter: &mut Emitter<'_>,
    array: NewArray<'_>,
    elements: &[u32],
    spreads: &[bool],
    code: &mut CodeBuilder,
) {
    let array_type = array.ty;
    if spreads.len() != elements.len() {
        emitter
            .run
            .set_emit_error("vararg spread flags do not match the element list".to_string());
        return;
    }
    let ir = emitter.ir;
    let mut flat = Vec::with_capacity(elements.len());
    flatten_spread_literals(ir, elements, spreads, None, &mut flat);
    let elements = flat.iter().map(|element| element.value).collect::<Vec<_>>();
    let spreads = flat
        .iter()
        .map(|element| element.spread)
        .collect::<Vec<_>>();
    let components = flat
        .iter()
        .map(|element| element.component)
        .collect::<Vec<_>>();
    let (elements, spreads) = (elements.as_slice(), spreads.as_slice());
    if !spreads.iter().any(|&spread| spread) {
        emit_packed_array(emitter, array, elements, &components, code);
        return;
    }

    let element_type = array_jvm_element(array_type);
    // A sole spread of an array built for this argument is that array. Every other sole spread is
    // `Arrays.copyOf(array, array.length)`, so the caller's array is not aliased.
    if elements.len() == 1
        && spreads[0]
        && matches!(
            ir.expr(elements[0]),
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
    if elements.len() == 1 && spreads[0] {
        if element_type.is_jvm_scalar() {
            emit_primitive_copy(emitter, element_type, elements[0], temps.as_deref(), code);
        } else {
            emit_reference_copy(emitter, array_type, elements[0], temps.as_deref(), code);
        }
    } else if element_type.is_jvm_scalar() {
        emit_primitive_spread(
            emitter,
            array,
            element_type,
            elements,
            spreads,
            temps.as_deref(),
            code,
        );
    } else {
        emit_reference_spread(
            emitter,
            array,
            element_type,
            Elements {
                values: elements,
                spreads,
                components: &components,
            },
            temps.as_deref(),
            code,
        );
    }
    if let Some(temps) = temps {
        emitter.release_operand_spills(&temps);
    }
}

/// One element of a vararg array after spread literals are flattened into it.
struct FlatElement {
    value: u32,
    spread: bool,
    /// The element type of the spread literal the value was written in, when it was. The value is
    /// adapted to that type, as the literal's own array would have stored it: an `Int` from
    /// `*arrayOf<Int>(1)` is boxed to `Integer` before it joins an `Object[]`.
    component: Option<Ty>,
}

/// The elements of a reference vararg array, with the type each one is adapted to.
struct Elements<'a> {
    values: &'a [u32],
    spreads: &'a [bool],
    components: &'a [Option<Ty>],
}

/// kotlinc's `VarargLowering` does not copy an array literal spread into a vararg
/// (`g(*arrayOf(a, b), c)`): the literal's elements join the enclosing array in place, under the
/// enclosing vararg's line. An empty literal keeps its own array, which is all an empty
/// `arrayOf()` and an `emptyArray()` have in common at this point.
fn flatten_spread_literals(
    ir: &IrFile,
    elements: &[u32],
    spreads: &[bool],
    component: Option<Ty>,
    flat: &mut Vec<FlatElement>,
) {
    for (&element, &spread) in elements.iter().zip(spreads) {
        match ir.expr(element) {
            IrExpr::Vararg {
                array_type,
                elements: literal,
                spreads: literal_spreads,
            } if spread && !literal.is_empty() && literal.len() == literal_spreads.len() => {
                let literal_component = Some(array_jvm_element(array_type));
                flatten_spread_literals(ir, literal, literal_spreads, literal_component, flat);
            }
            _ => flat.push(FlatElement {
                value: element,
                spread,
                component,
            }),
        }
    }
}

/// The scalar a reference array boxes element `index` to: the type of the literal it was
/// flattened from, or the array's own element type.
fn element_box_adapter(components: &[Option<Ty>], index: usize, element_type: Ty) -> Option<Ty> {
    let component = components.get(index).copied().flatten();
    reference_array_scalar_adapter(component.unwrap_or(element_type))
}

/// `Arrays.copyOf(array, array.length)` for one existing reference array. The erased overload
/// returns `Object[]`; a declaration whose physical vararg slot is narrower receives the exact
/// array checkcast before the call boundary.
fn emit_reference_copy(
    emitter: &mut Emitter<'_>,
    array_type: &Ty,
    element: u32,
    temps: Option<&[(u16, Ty, super::backend_temporaries::TemporaryLease)]>,
    code: &mut CodeBuilder,
) {
    place_array_and_length(emitter, element, temps, code);
    let copy = emitter.cw.methodref(
        "java/util/Arrays",
        "copyOf",
        "([Ljava/lang/Object;I)[Ljava/lang/Object;",
    );
    code.invokestatic(copy, 2, 1);
    let object_array = "[Ljava/lang/Object;";
    let target = type_descriptor(ir_ty_to_jvm(array_type));
    if target != object_array {
        let array_class = emitter.cw.class_ref(&target);
        code.checkcast(array_class);
    }
}

/// Emit a sole existing reference-array spread at a call boundary. `Arrays.copyOf(Object[], int)`
/// physically leaves `Object[]`; the call's ordinary operand adapter narrows it only when the
/// selected descriptor actually requires a more specific array class.
pub(super) fn emit_consumed_reference_copy(
    emitter: &mut Emitter<'_>,
    array_type: &Ty,
    elements: &[u32],
    spreads: &[bool],
    code: &mut CodeBuilder,
) -> Option<Ty> {
    let [element] = elements else {
        return None;
    };
    if spreads != [true]
        || array_jvm_element(array_type).is_jvm_scalar()
        || matches!(
            emitter.ir.expr(*element),
            IrExpr::Vararg { .. } | IrExpr::NewArray { .. }
        )
    {
        return None;
    }
    let temps = emitter
        .spills_operand_prefix(*element)
        .then(|| emitter.spill_to_temps(elements, code));
    place_array_and_length(emitter, *element, temps.as_deref(), code);
    let copy = emitter.cw.methodref(
        "java/util/Arrays",
        "copyOf",
        "([Ljava/lang/Object;I)[Ljava/lang/Object;",
    );
    code.invokestatic(copy, 2, 1);
    if let Some(temps) = temps {
        emitter.release_operand_spills(&temps);
    }
    Some(Ty::obj_args("kotlin/Array", &[Ty::obj("java/lang/Object")]))
}

fn emit_primitive_spread(
    emitter: &mut Emitter<'_>,
    array: NewArray<'_>,
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
        mark_array_line(emitter, array, code);
        load(builder_ty, builder_slot, code);
        if let Some(temps) = temps {
            let (slot, ty, _) = temps[index];
            load(ty, slot, code);
        } else {
            emitter.emit_value(element, code);
        }
        mark_array_line(emitter, array, code);
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
    mark_array_line(emitter, array, code);
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
    array: NewArray<'_>,
    element_type: Ty,
    elements: Elements<'_>,
    temps: Option<&[(u16, Ty, super::backend_temporaries::TemporaryLease)]>,
    code: &mut CodeBuilder,
) {
    let Elements {
        values: elements,
        spreads,
        components,
    } = elements;
    let builder = "kotlin/jvm/internal/SpreadBuilder";
    let class = emitter.cw.class_ref(builder);
    code.new_obj(class);
    code.dup();
    code.push_int(elements.len() as i32, emitter.cw);
    let init = emitter.cw.methodref(builder, "<init>", "(I)V");
    code.invokespecial(init, 1, 0);
    // kotlinc parks the builder in a local and reloads it for every `add` / `addSpread`, then
    // sizes the result array from it: `toArray(new T[builder.size()])`.
    let builder_ty = Ty::obj(builder);
    let held = emitter.frame.enter_temp(TempRole::VarargArray, builder_ty);
    let builder_slot = held.slot();
    store(builder_ty, builder_slot, code);
    let builder_lease = emitter.lease_frame_temporary(held, builder_ty);
    for (index, &element) in elements.iter().enumerate() {
        mark_array_line(emitter, array, code);
        load(builder_ty, builder_slot, code);
        if let Some(temps) = temps {
            let (slot, ty, _) = temps[index];
            load(ty, slot, code);
        } else {
            emitter.emit_value(element, code);
        }
        let produced = temps.map_or_else(|| emitter.value_ty(element), |temps| temps[index].1);
        let method = if spreads[index] {
            emitter
                .cw
                .methodref(builder, "addSpread", "(Ljava/lang/Object;)V")
        } else {
            let box_element = element_box_adapter(components, index, element_type);
            box_reference_scalar(emitter, box_element, produced, code);
            emitter
                .cw
                .methodref(builder, "add", "(Ljava/lang/Object;)V")
        };
        mark_array_line(emitter, array, code);
        code.invokevirtual(method, 1, 0);
    }
    mark_array_line(emitter, array, code);
    load(builder_ty, builder_slot, code);
    load(builder_ty, builder_slot, code);
    let size = emitter.cw.methodref(builder, "size", "()I");
    code.invokevirtual(size, 0, 1);
    emit_reference_new_array(emitter, element_type, array.reified_marker, code);
    let to_array = emitter.cw.methodref(
        builder,
        "toArray",
        "([Ljava/lang/Object;)[Ljava/lang/Object;",
    );
    code.invokevirtual(to_array, 1, 1);
    let array_class = emitter
        .cw
        .class_ref(&type_descriptor(ir_ty_to_jvm(array.ty)));
    code.checkcast(array_class);
    emitter.release_temporary(builder_lease);
}

/// Build a packed array through the next local slot, matching kotlinc's evaluation and frame shape.
pub(super) fn emit_packed_array(
    emitter: &mut Emitter<'_>,
    array: NewArray<'_>,
    elements: &[u32],
    components: &[Option<Ty>],
    code: &mut CodeBuilder,
) {
    let array_type = array.ty;
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
        emit_packed_array_through_temps(emitter, array, elements, components, code);
        return;
    }
    code.push_int(elements.len() as i32, emitter.cw);
    if element_type.is_jvm_scalar() && !reference_array {
        code.newarray(prim_newarray_atype(element_type));
    } else {
        // Nullability does not change the reference array class.
        emit_reference_new_array(emitter, element_type, array.reified_marker, code);
    }
    // No element is stored, so the fresh array stays on the stack. An `@InlineOnly` spread then
    // duplicates it (`aload; aload; arraylength`) instead of parking an empty array in a local.
    if elements.is_empty() {
        return;
    }

    let array_type = ir_ty_to_jvm(array_type);
    let temporary = emitter.frame.enter_temp(TempRole::VarargArray, array_type);
    let slot = temporary.slot();
    store(array_type, slot, code);
    let array_lease = emitter.lease_frame_temporary(temporary, array_type);

    let (store_op, width) = array_store_op(element_type, reference_array);
    for (index, &element) in elements.iter().enumerate() {
        let box_element = reference_array
            .then(|| element_box_adapter(components, index, element_type))
            .flatten();
        mark_array_line(emitter, array, code);
        load(array_type, slot, code);
        code.push_int(index as i32, emitter.cw);
        emitter.emit_value(element, code);
        // A nullable unsigned element is already the boxed value (or null). Boxing again calls
        // `box-impl` on a reference.
        box_reference_scalar(emitter, box_element, emitter.value_ty(element), code);
        code.array_store(store_op, width);
    }

    mark_array_line(emitter, array, code);
    load(array_type, slot, code);
    emitter.release_temporary(array_lease);
}

/// Build the same packed array with every element evaluated into a temp first. Used when any element
/// introduces control flow; see the caller for why the stack must be clean at that point.
fn emit_packed_array_through_temps(
    emitter: &mut Emitter<'_>,
    array: NewArray<'_>,
    elements: &[u32],
    components: &[Option<Ty>],
    code: &mut CodeBuilder,
) {
    let array_type = array.ty;
    let element_type = array_jvm_element(array_type);
    let reference_array = array_type.is_reference_array();
    let temps = emitter.spill_to_temps(elements, code);

    mark_array_line(emitter, array, code);
    code.push_int(elements.len() as i32, emitter.cw);
    if element_type.is_jvm_scalar() && !reference_array {
        code.newarray(prim_newarray_atype(element_type));
    } else {
        emit_reference_new_array(emitter, element_type, array.reified_marker, code);
    }

    let jvm_array_type = ir_ty_to_jvm(array_type);
    let temporary = emitter
        .frame
        .enter_temp(TempRole::VarargArray, jvm_array_type);
    let slot = temporary.slot();
    store(jvm_array_type, slot, code);
    let array_lease = emitter.lease_frame_temporary(temporary, jvm_array_type);

    let (store_op, width) = array_store_op(element_type, reference_array);
    for (index, &(temp_slot, temp_ty, _)) in temps.iter().enumerate() {
        let box_element = reference_array
            .then(|| element_box_adapter(components, index, element_type))
            .flatten();
        mark_array_line(emitter, array, code);
        load(jvm_array_type, slot, code);
        code.push_int(index as i32, emitter.cw);
        load(temp_ty, temp_slot, code);
        box_reference_scalar(emitter, box_element, temp_ty, code);
        code.array_store(store_op, width);
    }

    // Newest first: the array was entered above the element temporaries.
    mark_array_line(emitter, array, code);
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
