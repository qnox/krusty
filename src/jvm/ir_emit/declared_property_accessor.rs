//! A declared property's synthesized accessors: its default getter and setter.

use super::*;

/// The class whose property accessors are emitted, with what their bodies and signatures need.
pub(super) struct AccessorOwner<'a> {
    pub(super) ir: &'a IrFile,
    pub(super) class: &'a crate::ir::IrClass,
    pub(super) fq_name: &'a str,
    pub(super) formatter: &'a JvmSignatureFormatter<'a>,
    pub(super) param_assertions: bool,
}

/// Emit `property`'s synthesized accessor on `side`, unless the class declares that accessor itself.
pub(super) fn emit(
    owner: &AccessorOwner<'_>,
    property: &crate::ir::IrProperty,
    side: PropertyAccessorSide,
    cw: &mut ClassWriter,
) {
    let AccessorOwner {
        ir,
        class: c,
        fq_name,
        formatter,
        param_assertions,
    } = *owner;
    // `@JvmField` IS the declaration's realization: the field is the property's public face and
    // kotlinc emits no accessor beside it. Synthesizing one here would advertise a method the
    // metadata (correctly) never records.
    if property.is_private || is_jvm_field(c, &property.name) {
        return;
    }
    let Some(field_index) = property.backing_field else {
        return;
    };
    let Some(field) = c.fields.get(field_index as usize) else {
        return;
    };
    let type_parameter = ir
        .field_signatures(fq_name)
        .and_then(|signatures| {
            signatures
                .iter()
                .find(|(name, _)| *name == field.name)
                .map(|(_, parameter)| parameter.as_str())
        })
        .or(field.type_param.as_deref());
    let signatures = property_jvm_signatures(formatter, &field.ty, type_parameter);
    let field_jt = jvm_declared_ty(&field.ty);
    let field_desc = type_descriptor(field_jt);
    let accessor_jt = declared_property_accessor_jvm(ir, property, field);
    let accessor_desc = type_descriptor(accessor_jt);
    // Only an `open`/`override` PROPERTY's accessor is overridable — a plain `val` on an open
    // class keeps its FINAL accessor (kotlinc: `open class Engine(val name: String)` emits
    // `public final getName()`); Kotlin rejects overriding a non-open property, so the flag is
    // safe. Interface accessors stay non-final (their default bodies dispatch virtually).
    let overridable = property.is_open || c.is_interface;
    let getter = property
        .getter_jvm_name
        .clone()
        .unwrap_or_else(|| crate::names::property_getter_name(&property.name));
    let occupied = |name: &str, descriptor: &str| {
        c.methods.iter().any(|&fid| {
            let function = &ir.functions[fid as usize];
            function.name == name && ir_method_desc(&function.params, &function.ret) == descriptor
        })
    };
    let getter_desc = format!("(){accessor_desc}");
    if matches!(side, PropertyAccessorSide::Getter) && !occupied(&getter, &getter_desc) {
        // Visit the method header before constructing its code. This is especially observable for a
        // setter guard (`<set-?>`) and for a generic accessor Signature.
        let sig = &signatures.getter;
        let getter_ann = (accessor_jt.is_reference() && field.type_param.is_none()).then(|| {
            if property.ty.is_nullable() {
                "Lorg/jetbrains/annotations/Nullable;"
            } else {
                "Lorg/jetbrains/annotations/NotNull;"
            }
        });
        cw.reserve_method_pool(
            &getter,
            &getter_desc,
            sig.as_deref(),
            &getter_ann.into_iter().collect::<Vec<_>>(),
        );
        let mut g = CodeBuilder::new(1);
        let physical_name = instance_field_jvm_name(ir, c, field);
        let fref = cw.fieldref(fq_name, &physical_name, &field_desc);
        if static_storage(ir, c) {
            g.getstatic(fref, slot_words(field_jt) as i32);
        } else {
            g.aload(0);
            g.getfield(fref, slot_words(field_jt) as i32);
        }
        // A `lateinit var` read throws while the field is still null — kotlinc inserts this at every
        // access, and the accessor is an access like any other.
        if field.is_lateinit() {
            g.dup();
            let lbl = g.new_label();
            g.ifnonnull(lbl);
            g.push_string(&field.name, cw);
            let m = cw.methodref(
                "kotlin/jvm/internal/Intrinsics",
                "throwUninitializedPropertyAccessException",
                "(Ljava/lang/String;)V",
            );
            g.invokestatic(m, 1, 0);
            // The join needs a stackmap frame: `this` in local 0, the (non-null on the taken path)
            // field value on the stack.
            g.bind(lbl);
        }
        emit_backing_field_read_adaptation(ir, cw, &mut g, property, field_jt, accessor_jt);
        emit_return(accessor_jt, &mut g);
        g.ensure_locals(1);
        g.link();
        let access = default_accessor_access(property.visibility, overridable);
        cw.add_method_sig(access, &getter, &getter_desc, &g, sig.as_deref());
        seed_accessor_locals(c, fq_name, cw);
    }
    if matches!(side, PropertyAccessorSide::Setter) && property.is_var {
        let setter = property
            .setter_jvm_name
            .clone()
            .unwrap_or_else(|| crate::names::property_setter_name(&property.name));
        let setter_desc = format!("({accessor_desc})V");
        if !occupied(&setter, &setter_desc) {
            let sig = &signatures.setter;
            let setter_ann =
                (accessor_jt.is_reference() && field.type_param.is_none()).then(|| {
                    if property.ty.is_nullable() {
                        "Lorg/jetbrains/annotations/Nullable;"
                    } else {
                        "Lorg/jetbrains/annotations/NotNull;"
                    }
                });
            cw.reserve_method_pool(
                &setter,
                &setter_desc,
                sig.as_deref(),
                &setter_ann.into_iter().collect::<Vec<_>>(),
            );
            // `<set-?>` is the setter value parameter's JVM debug name even when no non-null guard
            // uses it as a String constant. Its UTF8 belongs to this method's header/debug window,
            // before the following declared member.
            cw.seed_utf8("<set-?>");
            let words = slot_words(accessor_jt);
            let mut st = CodeBuilder::new(1 + words);
            // kotlinc guards a non-null REFERENCE setter parameter, naming it `<set-?>`. A primitive
            // cannot be null, and neither is a type parameter that admits null (an unbounded `<T>` is
            // `Any?`); a NON-null-bounded one (`<T : Cargo>`) is guarded like any other reference.
            let guarded = param_assertions
                && accessor_jt.is_reference()
                && !property.ty.is_nullable()
                && is_nonnull_reference_field(ir, fq_name, &field.name, field.ty);
            if guarded {
                st.aload(1);
                st.push_string("<set-?>", cw);
                let m = cw.methodref(
                    "kotlin/jvm/internal/Intrinsics",
                    "checkNotNullParameter",
                    "(Ljava/lang/Object;Ljava/lang/String;)V",
                );
                st.invokestatic(m, 2, 0);
            }
            let statics_storage = static_storage(ir, c);
            if !statics_storage {
                st.aload(0);
            }
            load(accessor_jt, 1, &mut st);
            emit_backing_field_write_adaptation(ir, cw, &mut st, property, accessor_jt, field_jt);
            let physical_name = instance_field_jvm_name(ir, c, field);
            let fref = cw.fieldref(fq_name, &physical_name, &field_desc);
            if statics_storage {
                st.putstatic(fref, slot_words(field_jt) as i32);
            } else {
                st.putfield(fref, slot_words(field_jt) as i32);
            }
            st.ret_void();
            st.ensure_locals(1 + words);
            st.link();
            let access = default_accessor_access(property.setter_visibility, overridable);
            cw.add_method_sig(access, &setter, &setter_desc, &st, sig.as_deref());
            seed_accessor_locals(c, fq_name, cw);
        }
    }
}
