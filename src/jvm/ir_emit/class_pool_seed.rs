//! The constant-pool entries a plain class interns before krusty's emission reaches them, reserved
//! in kotlinc's visit order: the primary constructor's header and parameter annotations, its
//! LocalVariableTable and `$default` header.

use super::*;

pub(super) struct PlainClassPoolSeed<'a> {
    pub(super) ir: &'a IrFile,
    pub(super) class: &'a crate::ir::IrClass,
    pub(super) fq_name: &'a str,
    pub(super) ctor_signature: Option<&'a str>,
}

/// Reserve an ordinary class's constructor before its members, at kotlinc's schedule position.
pub(super) fn seed_ctor_before_members(
    enabled: bool,
    seed: PlainClassPoolSeed<'_>,
    cw: &mut ClassWriter,
) {
    if enabled && seed.class.has_primary_ctor && !seed.class.is_value {
        seed_plain_class_pool(seed, cw);
    }
}

/// Reserve a value class's constructor after the declared members which precede it. Reserving it
/// with an ordinary class makes `<init>` the first member-owned pool entry and shifts the whole pool
/// even when the method table is correctly ordered.
pub(super) fn seed_value_ctor_after_members(
    enabled: bool,
    seed: PlainClassPoolSeed<'_>,
    cw: &mut ClassWriter,
) {
    if enabled && seed.class.has_primary_ctor && seed.class.is_value {
        seed_plain_class_pool(seed, cw);
    }
}

fn seed_plain_class_pool(seed: PlainClassPoolSeed<'_>, cw: &mut ClassWriter) {
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
pub(super) fn seed_plain_constructor_tail(seed: PlainClassPoolSeed<'_>, cw: &mut ClassWriter) {
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
    let parameter_locals = constructor_parameter_locals(ir, c);
    cw.seed_plain_constructor_tail(fq_name, &parameter_locals, default_marker_desc.as_deref());
}

/// Seed an enum constructor's LocalVariableTable strings, which kotlinc interns with the constructor,
/// before `values`: `this`, the two synthetic `Enum` parameters, then every declared parameter.
pub(super) fn seed_enum_constructor_locals(
    ir: &IrFile,
    c: &crate::ir::IrClass,
    self_desc: &str,
    cw: &mut ClassWriter,
) {
    let synthetic = [
        ("this".to_string(), self_desc.to_string()),
        ("$enum$name".to_string(), "Ljava/lang/String;".to_string()),
        ("$enum$ordinal".to_string(), "I".to_string()),
    ];
    for (name, desc) in synthetic
        .into_iter()
        .chain(constructor_parameter_locals(ir, c))
    {
        cw.reserve_method_name(&name);
        cw.reserve_descriptor(&desc);
    }
}

/// The constructor's LocalVariableTable rows after its receiver and synthetic prefix: every named
/// parameter, property-backed or plain (`enum class E(value: Int)` has no field `value`, but its
/// constructor still lists it).
fn constructor_parameter_locals(ir: &IrFile, c: &crate::ir::IrClass) -> Vec<(String, String)> {
    c.ctor_args
        .iter()
        .zip(crate::jvm::parameter_names::constructor_local_variables(
            ir, c,
        ))
        .filter_map(|(argument, name)| {
            Some((name?, crate::jvm::names::type_descriptor(argument.ty)))
        })
        .collect()
}

/// Seed a synthesized accessor's `LocalVariableTable` receiver strings right after its body, where
/// kotlinc interns them. The table itself is attached later with the class's other synthesized
/// debug tables, which only a class with a declaration line gets; a primary constructor's tail has
/// usually interned these strings already.
pub(super) fn seed_accessor_locals(c: &crate::ir::IrClass, fq_name: &str, cw: &mut ClassWriter) {
    if c.decl_line != 0 {
        cw.seed_utf8("this");
        cw.seed_utf8(&format!("L{fq_name};"));
    }
}

/// Intern one generated value-class method's local names immediately after that method's body.
///
/// The debug tables are attached once the whole class has been emitted, but kotlinc visits each
/// table with its owning method. Reserving these strings at the same boundary keeps later method,
/// bridge, field, and annotation constants in their real order. Roles come from synthesis-owned
/// function identities; generated spellings are output here, never lookup input.
pub(super) fn seed_value_class_method_locals(
    enabled: bool,
    ir: &IrFile,
    c: &crate::ir::IrClass,
    fid: u32,
    cw: &mut ClassWriter,
) {
    if !enabled || !c.is_value {
        return;
    }
    let Some(field) = c.fields.first() else {
        return;
    };
    let carrier = crate::jvm::names::type_descriptor(field.ty);
    let this_desc = format!("L{};", c.fq_name());
    let object = "Ljava/lang/Object;";
    let locals: Vec<(&str, &str)> = if let Some(role) = ir.jvm_value_class_generated_any.get(&fid) {
        match role {
            crate::ir::IrValueClassAnyMember::Equals => {
                vec![("arg0", carrier.as_str()), ("other", object)]
            }
            crate::ir::IrValueClassAnyMember::HashCode
            | crate::ir::IrValueClassAnyMember::ToString => vec![("arg0", carrier.as_str())],
        }
    } else if let Some(role) = ir.jvm_value_class_any_delegators.get(&fid) {
        match role {
            crate::ir::IrValueClassAnyMember::Equals => {
                vec![("this", this_desc.as_str()), ("other", object)]
            }
            crate::ir::IrValueClassAnyMember::HashCode
            | crate::ir::IrValueClassAnyMember::ToString => {
                vec![("this", this_desc.as_str())]
            }
        }
    } else {
        match ir.jvm_value_class_representation_order.get(&fid).copied() {
            Some(0) => vec![(field.name.as_str(), carrier.as_str())],
            Some(1) => vec![("v", carrier.as_str())],
            Some(2) => vec![("this", this_desc.as_str())],
            Some(3) => vec![("p1", carrier.as_str()), ("p2", carrier.as_str())],
            _ => return,
        }
    };
    for (name, descriptor) in locals {
        cw.seed_utf8(name);
        cw.seed_utf8(descriptor);
    }
}
