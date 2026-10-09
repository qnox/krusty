//! JVM storage and `<clinit>` emission for a named object or interface companion.

use super::*;

pub(super) fn emit(
    ir: &IrFile,
    c: &IrClass,
    facade: &str,
    env: &EmitEnv,
    signature_formatter: &JvmSignatureFormatter<'_>,
    fq_name: &str,
    cw: &mut ClassWriter,
) {
    let clinit_statics: Vec<(u32, &crate::ir::IrStatic, crate::ir::ExprId)> = ir
        .statics
        .iter()
        .enumerate()
        .filter(|(_, property)| property.owner_matches(fq_name))
        .filter_map(|(index, property)| {
            Some((
                index as u32,
                property,
                static_fields::clinit_initializer(ir, property)?,
            ))
        })
        .collect();

    // An interface's companion self-hosts its singleton; a plain object publishes INSTANCE.
    let interface_companion = companion_of_interface(ir, c);
    let (instance_name, instance_access) = if interface_companion {
        ("$$INSTANCE", 0x1018) // STATIC | FINAL | SYNTHETIC (package-private)
    } else {
        ("INSTANCE", 0x0019) // PUBLIC | STATIC | FINAL
    };
    let self_desc = format!("L{fq_name};");

    // kotlinc reaches `<clinit>` before the fields, so the body interns first and the leading
    // INSTANCE and `$$delegatedProperties` intern with the field visit.
    cw.reserve_method_name("<clinit>");
    let nullability = (!interface_companion).then_some("Lorg/jetbrains/annotations/NotNull;");
    cw.add_field_late_leading(
        (instance_access, instance_name, &self_desc),
        None,
        nullability,
    );
    delegated_property_array::declare(env, c.fq_name, cw);

    // An interface companion interleaves const fields with instance fields in source order, and
    // both intern at the field-table visit (after `<clinit>` and the accessors). A named object's
    // backing fields stay on the eager path this block has always used.
    if interface_companion {
        emit_interface_companion_member_fields(ir, env.run, c, signature_formatter, fq_name, cw);
    } else {
        for (field_index, field) in c.fields.iter().enumerate() {
            let acc = declared_field_access(c, field_index, true);
            let type_parameter = ir
                .field_signatures(fq_name)
                .and_then(|signatures| signatures.iter().find(|(name, _)| *name == field.name))
                .map(|(_, parameter)| parameter.as_str())
                .or(field.type_param.as_deref());
            let field_sig =
                property_jvm_signatures(signature_formatter, &field.ty, type_parameter).field;
            let physical_name = instance_field_jvm_name(ir, env.run, c, field_index);
            cw.add_field_sig(
                acc,
                &physical_name,
                &ir_type_desc(&field.ty),
                field_sig.as_deref(),
            );
        }
    }

    let init_body = c.init_body;
    let mut emitter = Emitter::new(
        ir,
        &mut *cw,
        env,
        Some(StaticOwner::Class(c.fq_name)),
        fq_name,
        facade,
        Ty::Unit,
        clinit_statics
            .iter()
            .map(|&(_, _, init)| init)
            .chain(init_body),
    );
    emitter.regeneration_site = Some(
        super::bytecode_inline_call::RegenerationSite::class_initializer(env.signature_symbols),
    );
    emitter.record_locals = true;
    let mut clinit = CodeBuilder::new(0);
    emitter.emit_delegated_property_array(env, c.fq_name, fq_name, &mut clinit);
    let ci = emitter.cw.class_ref(fq_name);
    let init = emitter.cw.methodref(fq_name, "<init>", "()V");
    let fref = emitter.cw.fieldref(fq_name, instance_name, &self_desc);
    clinit.new_obj(ci);
    clinit.dup();
    clinit.invokespecial(init, 0, 0);
    clinit.putstatic(fref, 1);

    // Backend support storage must exist before source initializers. A generated declaration can
    // independently request placement after them; generated provenance alone does not imply order.
    let (after_source, before_source): (Vec<_>, Vec<_>) = clinit_statics
        .iter()
        .partition(|(index, _, _)| ir.static_initializer_is_after_source(*index));
    for &(index, _, init) in &before_source {
        emitter.emit_static_initializer_store(fq_name, index, init, &mut clinit);
    }

    let mut clinit_line_entries: Vec<(u16, u32)> = Vec::new();
    if let Some(init_body) = init_body {
        let body_start = clinit.bytes.len() as u16;
        if init_body_reads_this(ir, init_body) {
            let iref = emitter.cw.fieldref(fq_name, instance_name, &self_desc);
            clinit.getstatic(iref, 1);
            // The instance local opens a frame of its own: the stores before the source body
            // are done with every slot they took.
            emitter.frame = super::frame_map::FrameMap::default();
            let receiver = emitter.frame.enter(FrameKey::Receiver, Ty::obj(fq_name));
            store(Ty::obj(fq_name), receiver, &mut clinit);
            emitter.slots.insert(0, (receiver, Ty::obj(fq_name)));
        }
        let statements: Option<Vec<crate::ir::ExprId>> = match ir.expr(init_body) {
            crate::ir::IrExpr::Block { stmts, value } if value.is_none() => stmts
                .iter()
                .all(|&statement| matches!(ir.expr(statement), crate::ir::IrExpr::SetField { .. }))
                .then(|| stmts.clone()),
            _ => None,
        };
        match statements {
            Some(statements) => {
                for statement in statements {
                    let pc = clinit.bytes.len() as u16;
                    if let crate::ir::IrExpr::SetField { index, .. } = ir.expr(statement) {
                        let name = &c.fields[*index as usize].name;
                        if let Some(&line) = ir.prop_decl_lines.get(&(c.fq_name_id(), name.clone()))
                        {
                            if line != 0 {
                                clinit_line_entries.push((pc, line));
                            }
                        }
                    }
                    emitter.emit(statement, &mut clinit);
                }
            }
            // A block with `init` statements marks its own lines as it goes; keep them.
            None => {
                let first = clinit.line_marks().len();
                emitter.render_initializer_boundaries = true;
                emitter.emit(init_body, &mut clinit);
                emitter.render_initializer_boundaries = false;
                let marks = clinit.line_marks()[first..].iter();
                clinit_line_entries.extend(marks.map(|&(pc, line)| (pc, u32::from(line))));
            }
        }
        clinit_line_entries.dedup_by_key(|(_, line)| *line);
        if !c.is_source_declared && c.decl_line != 0 && c.decl_end_line != 0 {
            let start = if c.decl_start_line == 0 {
                c.decl_line
            } else {
                c.decl_start_line
            };
            clinit_line_entries = vec![(body_start, start)];
        }
    }

    for &(index, property, init) in &after_source {
        if property.line != 0
            && clinit_line_entries.last().map(|&(_, line)| line) != Some(property.line)
        {
            // Through the builder, so a line the source body left occupied at this offset (an
            // `init` block's closing `}`) keeps its entry behind a `nop`.
            clinit.mark_line(property.line);
            clinit_line_entries.push((clinit.bytes.len() as u16, property.line));
        }
        emitter.emit_static_initializer_store(fq_name, index, init, &mut clinit);
    }

    let clinit_max = emitter.frame.max();
    let clinit_return = clinit.bytes.len() as u16;
    clinit.ret_void();
    clinit.ensure_locals(clinit_max);
    clinit.link();
    cw.add_method(0x0008, "<clinit>", "()V", &clinit);

    // A one-line source object already maps its generated store to the closing line.
    let returns_to_closing_line = if c.is_source_declared {
        !after_source.is_empty()
            && clinit_line_entries
                .last()
                .is_some_and(|&(_, line)| line != c.decl_end_line)
    } else {
        !clinit_line_entries.is_empty()
    };
    if returns_to_closing_line && c.decl_end_line != 0 {
        clinit_line_entries.push((clinit_return, c.decl_end_line));
    }
    if !clinit_line_entries.is_empty() {
        cw.set_method_lines("<clinit>", "()V", &clinit_line_entries);
    }
}

/// Instance backing fields and companion-owned `const val`s, in source order, after `$$INSTANCE`.
///
/// A public or internal const also has a copy on the interface. The copy that stays here is the
/// companion's own field: private when the declaration is private, public otherwise (`internal` has
/// no JVM spelling). Each reference field carries the same nullability annotation as any other
/// backing field, attached here so its name is not interned ahead of the field visit.
fn emit_interface_companion_member_fields(
    ir: &IrFile,
    run: &EmitRun,
    c: &IrClass,
    signature_formatter: &JvmSignatureFormatter<'_>,
    fq_name: &str,
    cw: &mut ClassWriter,
) {
    enum Member {
        Instance(usize),
        Const(u32),
    }
    let property_order = |field_index: usize| {
        c.properties
            .iter()
            .find(|property| property.backing_field == Some(field_index as u32))
            .map(|property| property.source_order)
            .unwrap_or(u32::MAX)
    };
    let mut members = Vec::new();
    for (field_index, _) in c.fields.iter().enumerate() {
        members.push((
            property_order(field_index),
            field_index as u32,
            Member::Instance(field_index),
        ));
    }
    for (static_index, property) in ir.statics.iter().enumerate() {
        if property.is_const && property.owner_matches(fq_name) {
            members.push((
                property.source_order,
                static_index as u32,
                Member::Const(static_index as u32),
            ));
        }
    }
    members.sort_by_key(|(order, tie, _)| (*order, *tie));
    for (_, _, member) in members {
        match member {
            Member::Instance(field_index) => {
                let field = &c.fields[field_index];
                let acc = declared_field_access(c, field_index, true);
                let type_parameter = ir
                    .field_signatures(fq_name)
                    .and_then(|signatures| signatures.iter().find(|(name, _)| *name == field.name))
                    .map(|(_, parameter)| parameter.as_str())
                    .or(field.type_param.as_deref());
                let field_sig =
                    property_jvm_signatures(signature_formatter, &field.ty, type_parameter).field;
                let physical_name = instance_field_jvm_name(ir, run, c, field_index);
                let descriptor = ir_type_desc(&field.ty);
                let nullability = field_visibility::publishes_field_nullability(c, field_index)
                    .then(|| {
                        nullability_annotation(field_nullability_kind(
                            ir,
                            fq_name,
                            &field.name,
                            field.ty,
                        ))
                    })
                    .flatten();
                cw.add_field_late_sig(
                    acc,
                    &physical_name,
                    &descriptor,
                    field_sig.as_deref(),
                    None,
                    nullability,
                );
                if let Some(annotations) = c
                    .field_annotations
                    .iter()
                    .find(|annotations| annotations.field == field.name)
                {
                    cw.set_last_late_field_annotations(&annotations.annotations);
                }
            }
            Member::Const(static_index) => {
                let property = &ir.statics[static_index as usize];
                let descriptor = ir_type_desc(&property.ty);
                let access = if property.visibility.is_private() {
                    0x001a // PRIVATE | STATIC | FINAL
                } else {
                    0x0019 // PUBLIC | STATIC | FINAL
                };
                let value = property
                    .init
                    .and_then(|init| static_fields::constant_value(ir, init));
                let nullability = (descriptor.starts_with('L') || descriptor.starts_with('['))
                    .then(|| {
                        if property.ty.is_nullable() {
                            "Lorg/jetbrains/annotations/Nullable;"
                        } else {
                            "Lorg/jetbrains/annotations/NotNull;"
                        }
                    });
                cw.add_field_late(access, &property.name, &descriptor, value, nullability);
            }
        }
    }
}
