//! Lifting a lambda that captures values out of an anonymous-object copy's constructor: each
//! capture becomes a field of the copy, stored from the caller's capture operand, and every call
//! of the lambda's field is replaced by its body, spliced the way an ordinary inline call places
//! one. A lambda that takes parameters of its own is lifted once the copy's constructor code is
//! complete, because only then is it known whether that code reads it from its parameter.

use super::*;

/// Lift each capturing lambda that takes parameters of its own, once the copy's constructor code
/// is complete: a lambda that code reads from its parameter stays a value the code can invoke, and
/// that is only known after the declaration's initializers are cloned. Each call of the lambda's
/// field then binds its arguments to named locals of the spliced body, as an ordinary inline call
/// does.
pub(super) fn consume_parameterized_lambdas(
    ir: &mut crate::ir::IrFile,
    copy_id: ClassId,
) -> Result<(), super::super::super::FirLoweringFailure> {
    let Some(spec) = ir.specialized_anonymous_classes.get(&copy_id).cloned() else {
        return Ok(());
    };
    let class_name = ir.classes[copy_id as usize].fq_name;
    let construction = spec.order;
    let mut roots = spec.pending_property_roots.clone();
    roots.extend(
        ir.classes[copy_id as usize]
            .properties
            .iter()
            .filter_map(|property| property.initializer),
    );
    let mut lifts = Vec::new();
    for lambda in inline_lambda_arguments(ir, copy_id, construction) {
        let parameters = ir
            .functions
            .get(lambda.impl_fn as usize)
            .ok_or_else(|| malformed(class_name))?
            .params
            .len();
        if lambda.capture_count == 0
            || parameters <= lambda.capture_count
            || ir.suspend_funs.contains(&lambda.impl_fn)
            || constructor_reads_parameter(ir, copy_id, &roots, lambda.parameter)
        {
            continue;
        }
        lifts.push(lambda);
    }
    lifts.sort_by_key(|lambda| lambda.parameter);
    let before = ir.classes[copy_id as usize].fields.len();
    for lambda in lifts.iter().rev() {
        lift_capturing_lambda(
            ir,
            copy_id,
            construction,
            &roots,
            lambda,
            class_name,
            &spec.inline_callee_source_name,
        )?;
    }
    let after = ir.classes[copy_id as usize].fields.len();
    let omitted = u32::try_from(before.saturating_sub(after)).map_err(|_| malformed(class_name))?;
    let record = ir
        .specialized_anonymous_classes
        .get_mut(&copy_id)
        .ok_or_else(|| malformed(class_name))?;
    record.omitted_capture_fields += omitted;
    record.field_count =
        u32::try_from(after).map_err(|_| malformed(class_name))? + record.omitted_capture_fields;
    Ok(())
}

/// Replace one capturing lambda with one stored field per capture, and splice its body at each
/// zero-argument call of that field. The declaration class keeps the original lambda function.
pub(super) fn lift_capturing_lambda(
    ir: &mut crate::ir::IrFile,
    class: ClassId,
    construction: ExprId,
    pending_property_roots: &[ExprId],
    lambda: &InlineLambdaArgument,
    class_name: TypeName,
    inline_callee: &str,
) -> Result<(), super::super::super::FirLoweringFailure> {
    let IrExpr::Lambda { captures, .. } = ir.expr(lambda.expression).clone() else {
        return Err(malformed(class_name));
    };
    if captures.len() != lambda.capture_count {
        return Err(malformed(class_name));
    }
    let types = ir
        .functions
        .get(lambda.impl_fn as usize)
        .ok_or_else(|| malformed(class_name))?
        .params
        .clone();
    if types.len() < lambda.capture_count {
        return Err(malformed(class_name));
    }
    let identities = ir
        .function_parameter_identities(lambda.impl_fn)
        .map(<[crate::ir::IrParameterIdentity]>::to_vec);
    let mut names = Vec::with_capacity(lambda.capture_count);
    for index in 0..lambda.capture_count {
        let Some(name) = identities
            .as_ref()
            .and_then(|identities| identities.get(index))
            .and_then(|identity| identity.source_name.clone())
        else {
            return Ok(());
        };
        names.push(name);
    }
    let Some(invocations) =
        capturing_field_invocations(ir, class, pending_property_roots, lambda.field)
    else {
        return Ok(());
    };
    // The lifted captures' fields follow the prefix's. Every field must be a prefix value's
    // field, so the next field index is past them; a prefix value the constructor keeps in no
    // field (a capture only a super-constructor argument reads) has none.
    let declaration = &ir.classes[class as usize];
    let prefix_fields = declaration
        .ctor_args
        .iter()
        .take(declaration.constructor_prefix_count as usize)
        .filter(|argument| argument.field_index.is_some())
        .count();
    if declaration.fields.len() != prefix_fields {
        return Ok(());
    }
    remove_capture(
        ir,
        class,
        construction,
        pending_property_roots,
        CaptureRemoval {
            parameter: lambda.parameter,
            field: Some(lambda.field),
            constructor_forest: &[],
        },
        class_name,
    )?;
    let field_base = append_lifted_captures(
        ir,
        class,
        construction,
        &captures,
        &types[..lambda.capture_count],
        &names,
        class_name,
    )?;
    record_lifted_shared_cells(
        ir,
        class,
        lambda.inline_body,
        field_base,
        lambda.capture_count,
    );
    for invocation in invocations {
        splice_lifted_body(ir, invocation, lambda, field_base, class, inline_callee)?;
    }
    Ok(())
}

/// Zero-argument calls of capture `field`, each with the receiver of that field read. `None` when
/// the field is also read some other way, so the lambda has to stay callable.
fn capturing_field_invocations(
    ir: &crate::ir::IrFile,
    class: ClassId,
    pending_property_roots: &[ExprId],
    field: u32,
) -> Option<Vec<(ExprId, ExprId)>> {
    let mut roots = ir.classes[class as usize]
        .methods
        .iter()
        .filter_map(|method| ir.functions.get(*method as usize)?.body)
        .collect::<Vec<_>>();
    roots.extend(ir.classes[class as usize].init_body);
    roots.extend(pending_property_roots.iter().copied());
    let mut pending = roots;
    let mut seen = HashSet::default();
    let mut invocations = Vec::new();
    let mut field_reads = HashSet::default();
    let mut invoked_reads = HashSet::default();
    while let Some(expression) = pending.pop() {
        if !seen.insert(expression) {
            continue;
        }
        if let IrExpr::Lambda { .. } = ir.expr(expression) {
            continue;
        }
        crate::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
        if let IrExpr::GetField {
            class: owner,
            index,
            ..
        } = ir.expr(expression)
        {
            if *owner == class && *index == field {
                field_reads.insert(expression);
            }
        }
        if let IrExpr::InvokeFunction { func, .. } = ir.expr(expression) {
            if let Some(read) = capture_field_read(ir, *func, class, field) {
                let IrExpr::GetField { receiver, .. } = ir.expr(read) else {
                    return None;
                };
                invoked_reads.insert(read);
                invocations.push((expression, *receiver));
            }
        }
    }
    (field_reads == invoked_reads && !invocations.is_empty()).then_some(invocations)
}

/// A mutable capture is a shared cell in the lambda body (`RefGet`/`RefSet` of the capture
/// parameter). The field keeps the element type here; the JVM realizes the `Ref` holder.
fn record_lifted_shared_cells(
    ir: &mut crate::ir::IrFile,
    class: ClassId,
    body: ExprId,
    field_base: u32,
    capture_count: usize,
) {
    let mut pending = vec![body];
    let mut seen = HashSet::default();
    while let Some(expression) = pending.pop() {
        if !seen.insert(expression) {
            continue;
        }
        if matches!(ir.expr(expression), IrExpr::Lambda { .. }) {
            continue;
        }
        crate::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
        let (elem, holder) = match ir.expr(expression) {
            IrExpr::RefGet { elem, holder } => (*elem, *holder),
            IrExpr::RefSet { elem, holder, .. } => (*elem, *holder),
            _ => continue,
        };
        let IrExpr::GetValue(slot) = ir.expr(holder) else {
            continue;
        };
        let slot = *slot;
        if slot as usize >= capture_count {
            continue;
        }
        ir.shared_class_capture_fields
            .insert((class, field_base + slot), elem);
    }
}

pub(super) fn append_lifted_captures(
    ir: &mut crate::ir::IrFile,
    class: ClassId,
    construction: ExprId,
    captures: &[ExprId],
    types: &[Ty],
    names: &[String],
    class_name: TypeName,
) -> Result<u32, super::super::super::FirLoweringFailure> {
    let (insert_at, field_base, added) = {
        let declaration = &mut ir.classes[class as usize];
        let insert_at = usize::try_from(declaration.constructor_prefix_count)
            .map_err(|_| malformed(class_name))?;
        if insert_at > declaration.ctor_args.len() {
            return Err(malformed(class_name));
        }
        let field_base =
            u32::try_from(declaration.fields.len()).map_err(|_| malformed(class_name))?;
        let added = u32::try_from(captures.len()).map_err(|_| malformed(class_name))?;
        for (param, _) in &mut declaration.pre_super_param_fields {
            if *param >= insert_at as u32 {
                *param += added;
            }
        }
        for (offset, (ty, name)) in types.iter().zip(names.iter()).enumerate() {
            let field_index =
                field_base + u32::try_from(offset).map_err(|_| malformed(class_name))?;
            declaration
                .fields
                .push(crate::ir::IrField::new(format!("${name}$inlined"), *ty).with_is_final(true));
            declaration.ctor_args.insert(
                insert_at + offset,
                crate::ir::IrCtorArg {
                    name: None,
                    context_kind: crate::types::ContextParameterKind::None,
                    ty: *ty,
                    declared_ty: None,
                    is_field: true,
                    field_index: Some(field_index),
                    has_default: false,
                    is_vararg: false,
                    type_param: None,
                    check: None,
                    anonymous_super_forward: None,
                    capture: Some(crate::ir::IrConstructorCapture {
                        source_name: format!("{name}$inlined").into(),
                        receiver: None,
                        crossinline_parameter: None,
                        inlined: true,
                    }),
                    provenance: crate::ir::IrCtorParameterProvenance::Capture,
                    capture_identity: None,
                },
            );
            if !declaration.ctor_param_annotations.is_empty() {
                declaration.ctor_param_annotations.insert(
                    insert_at + offset,
                    crate::ir::DeclarationAnnotations::default(),
                );
            }
            declaration.pre_super_param_fields.push((
                u32::try_from(insert_at + offset).map_err(|_| malformed(class_name))?,
                field_index,
            ));
        }
        declaration.ctor_param_count += added;
        declaration.constructor_prefix_count += added;
        (insert_at, field_base, added)
    };
    if let Some(record) = ir.specialized_anonymous_classes.get_mut(&class) {
        if insert_at > record.constructor_parameters.len() {
            return Err(malformed(class_name));
        }
        record.constructor_parameters.splice(
            insert_at..insert_at,
            std::iter::repeat_n(None, captures.len()),
        );
    }
    if let Some(order) = ir.class_capture_order.get_mut(&class) {
        // The recorded order is a permutation of the constructor prefix that existed before these
        // captures, in constructor-prefix identities, not field indices: a prefix value kept in no
        // field still has an identity. A shorter order is a partial record; extending it would
        // name an identity past the permutation.
        if order.len() == insert_at {
            let mut extended = order.to_vec();
            for offset in 0..captures.len() {
                extended
                    .push(u32::try_from(insert_at + offset).map_err(|_| malformed(class_name))?);
            }
            *order = extended.into_boxed_slice();
        }
    }
    // A lifted capture is the inliner's constructor argument, not a source read at the object
    // expression. A plain local or field load must not open its own line between `new` and
    // `invokespecial`; a capture that is itself a call keeps the line of that call.
    for &capture in captures {
        if matches!(
            ir.expr(capture),
            IrExpr::GetValue(_) | IrExpr::GetField { .. }
        ) {
            let _ = ir.take_source_line(capture);
            ir.mark_positionless(capture);
        }
    }
    let IrExpr::New {
        args,
        ctor_params,
        declared_params,
        default_prefix_count,
        ..
    } = &mut ir.exprs[construction as usize]
    else {
        return Err(malformed(class_name));
    };
    if insert_at > args.len() {
        return Err(malformed(class_name));
    }
    for (offset, capture) in captures.iter().copied().enumerate() {
        args.insert(insert_at + offset, capture);
    }
    if let Some(parameters) = ctor_params {
        if insert_at > parameters.len() {
            return Err(malformed(class_name));
        }
        for (offset, ty) in types.iter().cloned().enumerate() {
            parameters.insert(insert_at + offset, ty);
        }
    }
    if declared_params.is_some() {
        let mut parameters = declared_params
            .as_ref()
            .expect("the option was just tested")
            .to_vec();
        if insert_at > parameters.len() {
            return Err(malformed(class_name));
        }
        for (offset, ty) in types.iter().cloned().enumerate() {
            parameters.insert(insert_at + offset, ty);
        }
        *declared_params = Some(parameters.into_boxed_slice());
    }
    *default_prefix_count += added;
    Ok(field_base)
}

fn splice_lifted_body(
    ir: &mut crate::ir::IrFile,
    invocation: (ExprId, ExprId),
    lambda: &InlineLambdaArgument,
    field_base: u32,
    class: ClassId,
    inline_callee: &str,
) -> Result<(), super::super::super::FirLoweringFailure> {
    let (invocation, receiver) = invocation;
    let class_name = ir.classes[class as usize].fq_name;
    let (spliced, _) = crate::ir::clone_expression_dag(ir, lambda.inline_body);
    let capture_count = u32::try_from(lambda.capture_count).map_err(|_| malformed(class_name))?;
    // The call's arguments bind the lambda's own value parameters, after the captures, as the
    // ordinary inline splice binds them: each is a named local of the spliced body.
    let IrExpr::InvokeFunction { args, .. } = ir.expr(invocation).clone() else {
        return Err(malformed(class_name));
    };
    let parameter_types = ir
        .functions
        .get(lambda.impl_fn as usize)
        .ok_or_else(|| malformed(class_name))?
        .params
        .clone();
    if parameter_types.len() != lambda.capture_count + args.len() {
        return Err(malformed(class_name));
    }
    let identities = ir
        .function_parameter_identities(lambda.impl_fn)
        .map(<[crate::ir::IrParameterIdentity]>::to_vec);
    let temporary = ir.next_value_slot();
    let mut declarations = Vec::with_capacity(args.len());
    for (ordinal, argument) in args.iter().copied().enumerate() {
        let position = lambda.capture_count + ordinal;
        let source_name = identities
            .as_ref()
            .and_then(|identities| identities.get(position))
            .and_then(|identity| identity.source_name.clone());
        let declaration = ir.add_expr(IrExpr::Variable {
            index: temporary
                + capture_count
                + u32::try_from(ordinal).map_err(|_| malformed(class_name))?,
            ty: parameter_types[position],
            init: Some(argument),
            named: source_name.is_some(),
        });
        ir.call_operand_bindings.insert(declaration);
        if let Some(name) = source_name {
            ir.value_names.insert(declaration, name);
            ir.set_debug_local_provenance(
                declaration,
                crate::ir::IrDebugLocalProvenance::InlineLambdaParameter { depth: 0 },
            );
        }
        declarations.push(declaration);
    }
    let formal_count =
        capture_count + u32::try_from(args.len()).map_err(|_| malformed(class_name))?;
    let formal_slots = (0..formal_count)
        .map(|index| temporary + index)
        .collect::<Vec<_>>();
    let local_base = temporary + formal_count;
    super::super::super::source_calls::rehome_inline_body_values(
        ir,
        spliced,
        &formal_slots,
        local_base,
    )
    .ok_or_else(|| malformed(class_name))?;
    replace_lifted_capture_reads(
        ir,
        spliced,
        temporary,
        capture_count,
        receiver,
        class,
        field_base,
    );
    ir.exprs[invocation as usize] =
        spliced_lambda_frame(ir, spliced, lambda.impl_fn, inline_callee, declarations);
    // The closing `nop` is the lambda literal's last line (its `}`), as an ordinary splice marks.
    if let Some(end) = ir.end_line(lambda.expression) {
        ir.set_end_line(invocation, end);
    }
    // The call now ends an inlined body, after which kotlinc forgets the line it ran under.
    if let Some(origin) = ir.fir_origins.get(&invocation).copied() {
        ir.fir_origins.insert(
            invocation,
            crate::ir::IrNodeOrigin::Synthetic {
                cause: origin.fir_origin(),
                kind: crate::fir::SyntheticOriginKind::InlinedCall,
            },
        );
    }
    ir.suspend_calls.remove(&invocation);
    ir.suspend_call_overridden_results.remove(&invocation);
    Ok(())
}

/// A lambda body spliced into a regenerated object opens its own inline frame, as an inlined
/// lambda body does at an ordinary call: the marker local is named after the inline callable the
/// lambda was passed to and the lambda's own class.
pub(super) fn spliced_lambda_frame(
    ir: &mut crate::ir::IrFile,
    spliced: ExprId,
    implementation: FunId,
    inline_callee: &str,
    mut bindings: Vec<ExprId>,
) -> IrExpr {
    let marker = ir.add_expr(IrExpr::InlineFrameMarker);
    ir.value_names.insert(marker, inline_callee.to_owned());
    ir.set_debug_local_provenance(
        marker,
        crate::ir::IrDebugLocalProvenance::LambdaFrameMarker {
            implementation,
            depth: 0,
        },
    );
    // The spliced caller body is copied code: the holder classes it names are not mapped into
    // this class's `InnerClasses`, and its lines are not the declaration's.
    mark_inline_copy_tree(ir, spliced);
    // The parameter bindings precede the marker in the one frame, so the locals close together.
    bindings.push(marker);
    IrExpr::Block {
        stmts: bindings,
        value: Some(spliced),
    }
}

fn replace_lifted_capture_reads(
    ir: &mut crate::ir::IrFile,
    root: ExprId,
    temporary: u32,
    capture_count: u32,
    receiver: ExprId,
    class: ClassId,
    field_base: u32,
) {
    let mut pending = vec![root];
    let mut seen = HashSet::default();
    let mut reads = Vec::new();
    while let Some(expression) = pending.pop() {
        if !seen.insert(expression) {
            continue;
        }
        if let IrExpr::Lambda { .. } = ir.expr(expression) {
            continue;
        }
        crate::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
        if let IrExpr::GetValue(index) = ir.expr(expression) {
            if (temporary..temporary + capture_count).contains(index) {
                reads.push((expression, *index - temporary));
            }
        }
    }
    for (expression, offset) in reads {
        let (receiver, _) = crate::ir::clone_expression_dag(ir, receiver);
        // The field access keeps the capture use's line and marks before this receiver. The
        // cloned receiver is the regenerated class's `this`, not a source read, so a line copied
        // onto it would open a second entry on the same `aload`.
        clear_capture_receiver_lines(ir, receiver);
        ir.exprs[expression as usize] = IrExpr::GetField {
            receiver,
            class,
            index: field_base + offset,
        };
    }
}

/// A lifted capture's receiver is storage, not a source expression. Drop every line the clone
/// carried so only the field access marks.
fn clear_capture_receiver_lines(ir: &mut crate::ir::IrFile, receiver: ExprId) {
    let mut pending = vec![receiver];
    let mut seen = HashSet::default();
    while let Some(expression) = pending.pop() {
        if !seen.insert(expression) {
            continue;
        }
        let _ = ir.take_source_line(expression);
        ir.mark_positionless(expression);
        crate::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
    }
}
