//! The constant-pool entries a plain class interns before krusty's emission reaches them, reserved
//! in kotlinc's visit order: the primary constructor's header and parameter stores, its
//! LocalVariableTable and `$default` header, then a data class's synthesized members.

use super::*;

pub(super) struct PlainClassPoolSeed<'a, 'symbols> {
    pub(super) formatter: &'a JvmSignatureFormatter<'symbols>,
    pub(super) ir: &'a IrFile,
    pub(super) bodies: &'a dyn MethodBodies,
    pub(super) class: &'a crate::ir::IrClass,
    pub(super) fq_name: &'a str,
    pub(super) superclass: &'a str,
    pub(super) ctor_signature: Option<&'a str>,
}

pub(super) fn seed_plain_class_pool(seed: PlainClassPoolSeed<'_, '_>, cw: &mut ClassWriter) {
    let PlainClassPoolSeed {
        ir,
        class: c,
        fq_name,
        superclass,
        ctor_signature,
        ..
    } = seed;
    let desc = |t: Ty| crate::jvm::names::type_descriptor(t);
    // Reference-type annotation kind: 0 = primitive or bare type parameter (no annotation), 1 =
    // non-null reference (@NotNull + a `checkNotNullParameter` guard), 2 = nullable (@Nullable, no guard).
    let ann_kind = |name: &str, t: Ty| -> u8 { field_nullability_kind(ir, fq_name, name, t) };
    let ctor_desc = primary_ctor_descriptor(c);
    let fields: Vec<crate::jvm::classfile::SeedField> = c
        .fields
        .iter()
        .enumerate()
        .map(|(i, f)| crate::jvm::classfile::SeedField {
            name: instance_field_jvm_name(ir, c, f),
            desc: desc(f.ty),
            ann_kind: ann_kind(&f.name, f.ty),
            is_ctor_param: i < c.ctor_param_count as usize,
            visible_ann_types: ctor_param_ann_types(c, i, true),
            invisible_ann_types: ctor_param_ann_types(c, i, false),
        })
        .collect();
    let ctor_sig = ctor_signature;
    let (mut super_param_tys, _) = super_ctor_jvm_tys(ir, c, superclass);
    if let Some(defaults) = ir
        .super_constructor_default_arguments
        .get(&c.fq_name_id())
        .filter(|defaults| !defaults.is_empty())
    {
        super_param_tys = jvm_tys(&c.super_ctor_params);
        super_param_tys.extend(std::iter::repeat_n(
            Ty::Int,
            constructor_default_masks(defaults, c.super_ctor_params.len()).len(),
        ));
        super_param_tys.push(Ty::obj("kotlin/jvm/internal/DefaultConstructorMarker"));
    }
    let super_ctor_desc = ir
        .external_super_constructors
        .get(&c.fq_name_id())
        .and_then(|target| target.descriptor.clone())
        .unwrap_or_else(|| crate::jvm::names::method_descriptor(&super_param_tys, Ty::Unit));
    cw.seed_plain_class_pool(
        fq_name,
        superclass,
        (&ctor_desc, &super_ctor_desc),
        &fields,
        &crate::jvm::classfile::MemberSignatures {
            ctor: ctor_sig,
        },
        &{
            use crate::jvm::classfile::SeedSuperArg;
            fn collect(ir: &IrFile, expr: crate::ir::ExprId, entries: &mut Vec<SeedSuperArg>) {
                match ir.expr(init_operand(ir, expr)) {
                    IrExpr::Const(crate::ir::IrConst::String(s)) => {
                        entries.push(SeedSuperArg::Str(s.clone()));
                    }
                    IrExpr::New {
                        internal,
                        args,
                        ctor_params,
                        ctor_desc,
                        ..
                    } => {
                        let owner = internal.render();
                        entries.push(SeedSuperArg::Class(owner.clone()));
                        for &arg in args {
                            collect(ir, arg, entries);
                        }
                        let desc = if let Some(desc) = ctor_desc {
                            desc.clone()
                        } else if let Some(params) = ctor_params {
                            method_descriptor(&jvm_tys(params), Ty::Unit)
                        } else {
                            let class = ir.class_id_by_name(*internal).expect(
                                "checked construction without explicit parameters must name an IR class",
                            );
                            method_descriptor(
                                &class_ctor_jvm_tys(&ir.classes[class as usize]),
                                Ty::Unit,
                            )
                        };
                        entries.push(SeedSuperArg::Ctor { owner, desc });
                    }
                    IrExpr::Variable {
                        init: Some(value), ..
                    } => collect(ir, *value, entries),
                    _ => {}
                }
            }

            let mut entries: Vec<SeedSuperArg> = Vec::new();
            for &statement in &c.super_arg_prelude {
                collect(ir, statement, &mut entries);
            }
            for &arg in &c.super_args {
                collect(ir, arg, &mut entries);
            }
            entries
        },
        &primary_ctor_annotations(c),
    );
}

/// Seed what kotlinc interns once the primary constructor's body is done: its local-variable
/// strings and `$default` overload, then a data class's synthesized members.
pub(super) fn seed_plain_constructor_tail(seed: PlainClassPoolSeed<'_, '_>, cw: &mut ClassWriter) {
    let PlainClassPoolSeed {
        ir,
        class: c,
        fq_name,
        ..
    } = seed;
    // The header of the primary ctor's `$default` overload, which kotlinc writes right after it.
    let default_marker_desc = ir
        .class_ctor_defaults(fq_name)
        .filter(|defaults| defaults.iter().any(Option::is_some))
        .map(|defaults| {
            let source_parameter_count = defaults
                .len()
                .checked_sub(c.constructor_prefix_count as usize)
                .expect("constructor default prefix exceeds its parameters");
            let masks = "I".repeat(default_mask_count(source_parameter_count));
            format!(
                "({}{masks}Lkotlin/jvm/internal/DefaultConstructorMarker;)V",
                primary_ctor_parameter_descs(c)
            )
        });
    // The constructor's LocalVariableTable after `this`: every named parameter, property-backed or
    // plain, as the constructor's debug table lists them.
    let parameter_locals: Vec<(String, String)> = c
        .ctor_args
        .iter()
        .zip(crate::jvm::parameter_names::constructor_local_variables(
            &c.ctor_args,
        ))
        .filter_map(|(argument, name)| {
            Some((name?, crate::jvm::names::type_descriptor(argument.ty)))
        })
        .collect();
    cw.seed_plain_constructor_tail(fq_name, &parameter_locals, default_marker_desc.as_deref());
}

/// Seed a data class's synthesized members once its constructors are written: the primary, its
/// `$default` overload and its marker accessor each intern their own bodies first.
pub(super) fn seed_data_class_pool(seed: PlainClassPoolSeed<'_, '_>, cw: &mut ClassWriter) {
    let PlainClassPoolSeed {
        formatter,
        ir,
        bodies,
        class: c,
        fq_name,
        ctor_signature,
        ..
    } = seed;
    if !synthesizes_data_class_members(c) {
        return;
    }
    // Generic `Signature`s for PARAMETERIZED-type members (`List<String>` → `Ljava/util/List<Ljava/lang/String;>;`).
    // Only for a class with NO bare type-parameter fields — a generic class's bare-`T` members are handled by
    // the existing tparam path, left untouched. Seeded here so the natural emission (add_field_sig/
    // add_method_sig) dedupes to kotlinc's interning positions.
    // A field's generic `Signature`: a bare type parameter (`val a: T` → `TT;`), else a parameterized
    // concrete type (`List<String>`). Disjoint — a field is one or the other.
    let field_sig_of = |f: &crate::ir::IrField| -> Option<String> {
        let type_parameter = ir
            .field_signatures(fq_name)
            .and_then(|fs| {
                fs.iter()
                    .find(|(name, _)| name == &f.name)
                    .map(|(_, parameter)| parameter.as_str())
            })
            .or(f.type_param.as_deref());
        property_jvm_signatures(formatter, &f.ty, type_parameter).field
    };
    let field_sigs: Vec<Option<String>> = c.fields.iter().map(field_sig_of).collect();
    // A data class's accessor signatures join its accessor window below, while its backing-field
    // signatures land late after the synthesized data methods. Ordinary classes intern both naturally
    // at the exact accessor/field visits.
    // A companion OUTER's `access$…$cp` bridges, `<clinit>`, and hoisted-initializer constants are
    // NOT seeded here: kotlinc interns them at their natural emission position — after the declared
    // member methods (whose bodies intern their own constants in between) — so `emit_class` reserves
    // each name at its emission site instead.
    data_class_pool_seed::seed_data_class_members(
        data_class_pool_seed::DataClassPoolSeed {
            ir,
            class: c,
            bodies,
            fq_name,
            ctor_signature,
            ctor_desc: &primary_ctor_descriptor(c),
            field_sigs: &field_sigs,
            field_sig_of: &field_sig_of,
        },
        cw,
    );
}
