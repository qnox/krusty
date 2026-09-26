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
    let clinit_statics: Vec<(u32, &crate::ir::IrStatic)> = ir
        .statics
        .iter()
        .enumerate()
        .filter(|(_, property)| {
            property.owner_matches(fq_name)
                && !(property.is_const && static_fields::const_value_idx_peek(ir, property.init))
        })
        .map(|(index, property)| (index as u32, property))
        .collect();

    // An interface's companion self-hosts its singleton; a plain object publishes INSTANCE.
    let interface_companion = companion_of_interface(ir, c);
    let (instance_name, instance_access) = if interface_companion {
        ("$$INSTANCE", 0x1018) // STATIC | FINAL | SYNTHETIC (package-private)
    } else {
        ("INSTANCE", 0x0019) // PUBLIC | STATIC | FINAL
    };
    let self_desc = format!("L{fq_name};");

    // kotlinc reaches `<clinit>` before the INSTANCE field, so reserve and intern in body order.
    cw.reserve_method_name("<clinit>");
    let ci = cw.class_ref(fq_name);
    let init = cw.methodref(fq_name, "<init>", "()V");
    let fref = cw.fieldref(fq_name, instance_name, &self_desc);
    cw.add_field(instance_access, instance_name, &self_desc);
    if !interface_companion {
        cw.set_field_nullability("INSTANCE", "Lorg/jetbrains/annotations/NotNull;");
    }

    // Backing fields follow INSTANCE in the field table.
    for field in &c.fields {
        let private = field.is_private();
        let acc =
            jvm_field_visibility(c, &field.name).unwrap_or(if private { 0x0002 } else { 0x0001 })
                | if field.is_final() { 0x0010 } else { 0 }
                | 0x0008;
        let type_parameter = ir
            .field_signatures(fq_name)
            .and_then(|signatures| signatures.iter().find(|(name, _)| *name == field.name))
            .map(|(_, parameter)| parameter.as_str())
            .or(field.type_param.as_deref());
        let field_sig =
            property_jvm_signatures(signature_formatter, &field.ty, type_parameter).field;
        let physical_name = instance_field_jvm_name(ir, c, field);
        cw.add_field_sig(
            acc,
            &physical_name,
            &ir_type_desc(&field.ty),
            field_sig.as_deref(),
        );
    }

    let init_body = c.init_body;
    let mut emitter = Emitter::new(
        ir,
        &mut *cw,
        env,
        fq_name,
        facade,
        Ty::Unit,
        clinit_statics
            .iter()
            .map(|(_, property)| property.init)
            .chain(init_body),
    );
    let mut clinit = CodeBuilder::new(0);
    clinit.new_obj(ci);
    clinit.dup();
    clinit.invokespecial(init, 0, 0);
    clinit.putstatic(fref, 1);

    // Backend support storage must exist before source initializers. A generated declaration can
    // independently request placement after them; generated provenance alone does not imply order.
    let (after_source, before_source): (Vec<_>, Vec<_>) = clinit_statics
        .iter()
        .partition(|(index, _)| ir.static_initializer_is_after_source(*index));
    for (index, _) in &before_source {
        emitter.emit_static_initializer_store(fq_name, *index, &mut clinit);
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
            None => emitter.emit(init_body, &mut clinit),
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

    for (index, property) in &after_source {
        if property.line != 0
            && clinit_line_entries.last().map(|&(_, line)| line) != Some(property.line)
        {
            clinit_line_entries.push((clinit.bytes.len() as u16, property.line));
        }
        emitter.emit_static_initializer_store(fq_name, *index, &mut clinit);
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
