//! The constant-pool entries a plain class interns before krusty's emission reaches them, reserved
//! in kotlinc's visit order: the primary constructor's header and parameter annotations, its
//! LocalVariableTable and `$default` header, then a data class's synthesized members.

use super::*;

pub(super) struct PlainClassPoolSeed<'a, 'symbols> {
    pub(super) formatter: &'a JvmSignatureFormatter<'symbols>,
    pub(super) ir: &'a IrFile,
    pub(super) bodies: &'a dyn MethodBodies,
    pub(super) class: &'a crate::ir::IrClass,
    pub(super) fq_name: &'a str,
    pub(super) ctor_signature: Option<&'a str>,
}

pub(super) fn seed_plain_class_pool(seed: PlainClassPoolSeed<'_, '_>, cw: &mut ClassWriter) {
    let PlainClassPoolSeed {
        ir,
        class: c,
        ctor_signature,
        ..
    } = seed;
    let ctor_desc = primary_ctor_descriptor(c);
    let parameters: Vec<crate::jvm::classfile::SeedCtorParameter> =
        primary_ctor_source_parameters(ir, c)
            .into_iter()
            .map(|parameter| {
                let (visible, invisible) = parameter
                    .annotations
                    .map(crate::jvm::classfile::split_declaration_annotations)
                    .unwrap_or_default();
                let types = |annotations: Vec<crate::ir::AppliedAnnotation>| -> Vec<String> {
                    annotations
                        .iter()
                        .map(|annotation| format!("L{};", annotation.internal))
                        .collect()
                };
                crate::jvm::classfile::SeedCtorParameter {
                    ann_kind: parameter.nullability,
                    visible_ann_types: types(visible),
                    invisible_ann_types: types(invisible),
                }
            })
            .collect();
    cw.seed_plain_class_pool(
        &ctor_desc,
        &parameters,
        &crate::jvm::classfile::MemberSignatures {
            ctor: ctor_signature,
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
