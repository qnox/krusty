//! A Kotlin `annotation class` as its JVM annotation interface: one abstract accessor per element,
//! each with its declaration default, and the retention/target meta-annotations kotlinc stamps.

use super::*;

/// The `@java.lang.annotation.Target` mirror for an annotation class whose source declares
/// `@Target(...)`, or `None` when it declares none — an ABSENT `@Target` and an EMPTY one are
/// different classfiles (the latter mirrors to `value = []`, and kotlinc emits it).
///
/// The Kotlin target set is PROJECTED, not copied: [`crate::types::KotlinTarget::java_element_type`]
/// maps each target to at most one `ElementType`, Kotlin-only targets drop out, and the survivors are
/// deduplicated and ordered by `ElementType` declaration order (kotlinc builds an `EnumSet`). The
/// projection can be empty — a set of only Kotlin-only targets still mirrors to `value = []`, matching
/// kotlinc; it is the ABSENCE of `@Target` in the source, not an empty projection, that omits it.
fn java_target_mirror(
    applied: &crate::ir::DeclarationAnnotations,
) -> Option<crate::ir::AppliedAnnotation> {
    let kotlin_target = crate::types::type_name("kotlin/annotation/Target");
    let declared = applied
        .iter()
        .map(|retained| &retained.annotation)
        .find(|annotation| annotation.internal == kotlin_target)?;
    let (_, crate::ir::AnnoValue::Array(targets)) = declared
        .values
        .iter()
        .find(|(name, _)| name == "allowedTargets")?
    else {
        return None;
    };
    let mut elements: Vec<usize> = targets
        .iter()
        .filter_map(|target| match target {
            crate::ir::AnnoValue::Enum(owner, name) => {
                crate::types::KotlinTarget::of_entry(*owner, name)?.java_element_type()
            }
            _ => None,
        })
        .collect();
    elements.sort_unstable();
    elements.dedup();
    Some(crate::ir::AppliedAnnotation {
        internal: crate::types::type_name("java/lang/annotation/Target"),
        values: vec![(
            "value".to_string(),
            crate::ir::AnnoValue::Array(
                elements
                    .into_iter()
                    .map(|element| {
                        crate::ir::AnnoValue::Enum(
                            crate::types::type_name("java/lang/annotation/ElementType"),
                            crate::types::JAVA_ELEMENT_TYPES[element].to_string(),
                        )
                    })
                    .collect(),
            ),
        )],
    })
}

/// Emit a Kotlin `annotation class` as a JVM ANNOTATION INTERFACE: `ACC_PUBLIC|ACC_INTERFACE|ACC_ABSTRACT|
/// ACC_ANNOTATION`, extending `java/lang/annotation/Annotation`, with one `public abstract` accessor per
/// member (`int x()`, `String s()`) named after the property and returning its type — kotlinc's shape.
/// Members come from `fields`. Instances are built by the synthetic impl ([`emit_annotation_impl_class`]).
pub(super) fn emit_annotation_class(
    ir: &IrFile,
    c: &crate::ir::IrClass,
    facade: &str,
    env: &EmitEnv,
    opts: &EmitOptions,
    class_meta: Option<&KotlinMetadata>,
) -> Vec<u8> {
    let fq_name = c.fq_name();
    let mut cw = new_writer(&fq_name, "java/lang/Object", opts);
    // PUBLIC | INTERFACE | ABSTRACT | ANNOTATION
    cw.set_access(class_public_bit(ir, c) | 0x0200 | 0x0400 | 0x2000);
    cw.add_interface("java/lang/annotation/Annotation");
    let signature_formatter = JvmSignatureFormatter::new(ir, env);
    let element_defaults = ir.annotation_element_defaults.get(&c.fq_name_id());
    for field in &c.fields {
        // PUBLIC|ABSTRACT. A `KClass` member's descriptor is `java.lang.Class`; the signature
        // keeps the type argument.
        let (descriptor, signature) =
            annotation_impl::annotation_interface_member(&signature_formatter, field.ty);
        cw.add_abstract_method_sig(0x0401, &field.name, &descriptor, signature.as_deref());
        if let Some(default) = element_defaults
            .and_then(|defaults| defaults.iter().find(|(element, _)| *element == field.name))
        {
            cw.set_last_method_annotation_default(&default.1);
        }
    }
    emit_jvm_interface_companion_surface(ir, c, facade, env, &mut cw);
    // Retention/target meta-annotations, matching kotlinc's ORDER: everything the source declares
    // comes first, in source order — `kotlin.annotation.Retention(X)` and `kotlin.annotation.Target`
    // among them — and the JVM mirrors are appended after: `java.lang.annotation.Retention(RUNTIME|
    // CLASS|SOURCE)` (RUNTIME when defaulted), then `java.lang.annotation.Target`. The java retention
    // is what both the JVM and classpath consumers read the retention back from.
    let enum_stamp = |internal: &str, enum_ty: &str, constant: &str| crate::ir::AppliedAnnotation {
        internal: crate::types::type_name(internal),
        values: vec![(
            "value".to_string(),
            crate::ir::AnnoValue::Enum(crate::types::type_name(enum_ty), constant.to_string()),
        )],
    };
    let mut mirrors: Vec<crate::ir::AppliedAnnotation> = Vec::new();
    if let Some(retention) = c.annotation_retention {
        use crate::ir::AnnoRetention;
        let policy = match retention {
            AnnoRetention::Default | AnnoRetention::Runtime => "RUNTIME",
            AnnoRetention::Binary => "CLASS",
            AnnoRetention::Source => "SOURCE",
        };
        mirrors.push(enum_stamp(
            "java/lang/annotation/Retention",
            "java/lang/annotation/RetentionPolicy",
            policy,
        ));
    }
    mirrors.extend(java_target_mirror(&c.applied_annotations));
    // The source's own `@Retention` is REPLACED IN PLACE by the normalized stamp rather than filtered
    // out and re-appended: the class's `annotation_retention` is the authority for the value, but the
    // annotation's position among the others is the source's and kotlinc preserves it.
    let kotlin_retention = crate::types::type_name("kotlin/annotation/Retention");
    let kotlin_retention_stamp = c.annotation_retention.and_then(|retention| {
        use crate::ir::AnnoRetention;
        let constant = match retention {
            AnnoRetention::Default => return None,
            AnnoRetention::Runtime => "RUNTIME",
            AnnoRetention::Binary => "BINARY",
            AnnoRetention::Source => "SOURCE",
        };
        Some(enum_stamp(
            "kotlin/annotation/Retention",
            "kotlin/annotation/AnnotationRetention",
            constant,
        ))
    });
    let user_annotations = crate::ir::DeclarationAnnotations::new(
        c.applied_annotations
            .iter()
            .filter_map(|retained| {
                if retained.annotation.internal != kotlin_retention {
                    return Some(retained.clone());
                }
                kotlin_retention_stamp
                    .clone()
                    .map(|annotation| declaration_annotations::replace(retained, annotation))
            })
            .collect(),
    );
    cw.set_class_annotations(&user_annotations);
    cw.set_runtime_annotations(&mirrors);
    let computed = (class_meta.is_none() && opts.emit_class_metadata)
        .then(|| build_class_metadata(ir, c, opts, env))
        .flatten();
    if let Some(m) = class_meta.or(computed.as_ref()) {
        cw.set_kotlin_metadata(m.k, m.stamp, m.xi, &m.d1, &m.d2);
    }
    cw.finish()
}
