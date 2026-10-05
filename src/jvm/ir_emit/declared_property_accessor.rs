//! A declared property's synthesized accessors: its default getter and setter.

use super::*;

/// The class whose property accessors are emitted, with what their bodies and signatures need.
pub(super) struct AccessorOwner<'a> {
    pub(super) ir: &'a IrFile,
    pub(super) class: &'a crate::ir::IrClass,
    pub(super) fq_name: &'a str,
    pub(super) formatter: &'a JvmSignatureFormatter<'a>,
    pub(super) param_assertions: bool,
    pub(super) override_results: &'a crate::jvm::override_results::OverrideResults,
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
        override_results,
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
    // A scalar getter result over a reference-returning overridden getter is the wrapper; the
    // setter keeps taking the scalar (see `jvm::override_results`).
    let boxed_getter = c
        .properties
        .iter()
        .position(|declared| std::ptr::eq(declared, property))
        .is_some_and(|index| override_results.boxes_member_property(c.fq_name, index as u32));
    let getter_jt = if boxed_getter {
        jvm_declared_ty(&Ty::nullable(property.ty))
    } else {
        accessor_jt
    };
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
    let getter_desc = format!("(){}", type_descriptor(getter_jt));
    if matches!(side, PropertyAccessorSide::Getter) && !occupied(&getter, &getter_desc) {
        // Visit the method header before constructing its code. This is especially observable for a
        // setter guard (`<set-?>`) and for a generic accessor Signature.
        let sig = &signatures.getter;
        let getter_ann = (getter_jt.is_reference() && field.type_param.is_none()).then(|| {
            if property.ty.is_nullable() {
                "Lorg/jetbrains/annotations/Nullable;"
            } else {
                "Lorg/jetbrains/annotations/NotNull;"
            }
        });
        let annotations = &property.accessor_annotations.getter;
        cw.reserve_method_pool_with_annotations(
            &getter,
            &getter_desc,
            sig.as_deref(),
            &getter_ann.into_iter().collect::<Vec<_>>(),
            annotations,
            &[],
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
        // kotlinc's `LateinitLowering` getter: return the value, or throw and then
        // `aconst_null; areturn` so the null path has a reference for the verifier.
        if field.is_lateinit() {
            g.dup();
            let missing = g.new_label();
            g.ifnull(missing);
            emit_backing_field_read_adaptation(ir, cw, &mut g, property, field_jt, getter_jt);
            emit_return(getter_jt, &mut g);
            g.bind(missing);
            g.pop();
            g.push_string(&field.name, cw);
            let m = cw.methodref(
                "kotlin/jvm/internal/Intrinsics",
                "throwUninitializedPropertyAccessException",
                "(Ljava/lang/String;)V",
            );
            g.invokestatic(m, 1, 0);
            g.aconst_null();
            emit_return(getter_jt, &mut g);
        } else {
            emit_backing_field_read_adaptation(ir, cw, &mut g, property, field_jt, getter_jt);
            emit_return(getter_jt, &mut g);
        }
        g.ensure_locals(1);
        g.link();
        let access = default_accessor_access(property.visibility, overridable);
        cw.add_method_sig(access, &getter, &getter_desc, &g, sig.as_deref());
        super::function_annotations::emit_declared(cw, annotations, &getter, &getter_desc);
        // The class-wide pass annotates accessors by their backing field's type, which a boxed
        // getter result does not have.
        if boxed_getter {
            cw.set_method_nullability(&getter, &getter_desc, getter_ann, &[]);
        }
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
            let annotations = &property.accessor_annotations.setter;
            cw.reserve_method_pool_with_annotations(
                &setter,
                &setter_desc,
                sig.as_deref(),
                &setter_ann.into_iter().collect::<Vec<_>>(),
                annotations,
                &[],
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
            super::function_annotations::emit_declared(cw, annotations, &setter, &setter_desc);
            seed_accessor_locals(c, fq_name, cw);
        }
    }
}

/// JVM type exposed by a synthesized property accessor. The value-class pass stamps the exact
/// mangled getter only when this property's own type uses its erased field carrier. Consume that
/// decision directly; emission must not repeat classifier/value-class lookup. An explicit backing
/// field with a narrower type does not otherwise change the property's public descriptor.
pub(super) fn declared_property_accessor_jvm(
    _ir: &IrFile,
    property: &crate::ir::IrProperty,
    field: &crate::ir::IrField,
) -> Ty {
    if property.getter_jvm_name.is_some() {
        jvm_declared_ty(&field.ty)
    } else {
        jvm_declared_ty(&stored_value_ty(property.ty))
    }
}

/// Adapt the physical backing-field value already on the stack to the property's declared return.
/// Resolution has already chosen both types; this is only their JVM representation boundary.
pub(super) fn emit_backing_field_read_adaptation(
    ir: &IrFile,
    cw: &mut ClassWriter,
    code: &mut CodeBuilder,
    property: &crate::ir::IrProperty,
    field_jvm: Ty,
    accessor_jvm: Ty,
) {
    if field_jvm == accessor_jvm {
        return;
    }
    if field_jvm.is_reference() && accessor_jvm.is_jvm_scalar() {
        unbox_prim_from(cw, code, field_jvm, accessor_jvm);
    } else if accessor_jvm.is_reference() {
        if let Some(storage) = property
            .storage_ty
            .and_then(|ty| ty.non_null().obj_internal())
        {
            if crate::jvm::value_classes::is_boxed_value_class(ir, storage)
                && field_jvm.is_jvm_scalar()
            {
                emit_box_impl(ir, cw, &Ty::obj_name(storage), code);
                return;
            }
        }
        if field_jvm.is_jvm_scalar() {
            box_prim_free(cw, code, field_jvm);
        } else {
            let internal = crate::jvm::names::instanceof_internal_name(accessor_jvm);
            if internal != "java/lang/Object" {
                let class = cw.class_ref(&internal);
                code.checkcast(class);
            }
        }
    }
}

/// Adapt a synthesized setter's declared argument to the physical backing-field representation.
pub(super) fn emit_backing_field_write_adaptation(
    ir: &IrFile,
    cw: &mut ClassWriter,
    code: &mut CodeBuilder,
    property: &crate::ir::IrProperty,
    accessor_jvm: Ty,
    field_jvm: Ty,
) {
    if accessor_jvm == field_jvm {
        return;
    }
    if accessor_jvm.is_reference() && field_jvm.is_jvm_scalar() {
        if let Some(storage) = property
            .storage_ty
            .and_then(|ty| ty.non_null().obj_internal())
        {
            if crate::jvm::value_classes::is_boxed_value_class(ir, storage) {
                let class = cw.class_ref(&storage.render());
                code.checkcast(class);
                emit_unbox_impl(ir, cw, &Ty::obj_name(storage), code);
                return;
            }
        }
        unbox_prim_from(cw, code, accessor_jvm, field_jvm);
    } else if accessor_jvm.is_jvm_scalar() && field_jvm.is_reference() {
        box_prim_free(cw, code, accessor_jvm);
    } else if accessor_jvm.is_reference() && field_jvm.is_reference() {
        let internal = crate::jvm::names::instanceof_internal_name(field_jvm);
        if internal != "java/lang/Object" {
            let class = cw.class_ref(&internal);
            code.checkcast(class);
        }
    }
}
