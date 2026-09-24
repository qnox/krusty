//! Source-annotation queries and annotations synthesized on serialization-plugin declarations.

use crate::ir::{
    AnnoValue, AppliedAnnotation, ClassId, DeclarationAnnotations, IrConst, IrFile,
    RetainedAnnotation,
};
use crate::kt_string::KtString;
use crate::plugins::PluginContext;
use crate::types::{type_name, AnnotationRetention};

use super::SERIALIZABLE_FQ;

const SERIAL_NAME_FQ: &str = "kotlinx/serialization/SerialName";
const CONTEXTUAL_FQ: &str = "kotlinx/serialization/Contextual";
const USE_CONTEXTUAL_SERIALIZATION_FQ: &str = "kotlinx/serialization/UseContextualSerialization";

/// `write$Self` is a Kotlin static helper, not merely a static JVM realization.
pub(super) fn write_self_annotations() -> DeclarationAnnotations {
    DeclarationAnnotations::new(vec![RetainedAnnotation {
        retention: AnnotationRetention::Runtime,
        annotation: AppliedAnnotation {
            internal: type_name("kotlin/jvm/JvmStatic"),
            values: Vec::new(),
        },
    }])
}

pub(super) fn custom_serializer_of(
    ctx: &PluginContext,
    ir: &IrFile,
    class_id: ClassId,
) -> Option<crate::types::TypeName> {
    ctx.class_annotation_class_literal(ir, class_id, type_name(SERIALIZABLE_FQ))
}

pub(super) fn field_serializer_of(
    ctx: &PluginContext,
    ir: &IrFile,
    class_id: ClassId,
    property: &str,
) -> Option<crate::types::TypeName> {
    ctx.property_annotation_class_literal(ir, class_id, property, type_name(SERIALIZABLE_FQ))
}

pub(super) fn serial_name_of(
    ctx: &PluginContext,
    ir: &IrFile,
    class_id: ClassId,
    property: &str,
) -> Option<KtString> {
    ctx.property_annotation_const_string(ir, class_id, property, type_name(SERIAL_NAME_FQ))
}

/// The class name used in the serial form: a declared `@SerialName("…")`, or the stable qualified
/// source name recorded by common lowering. The plugin must not reinterpret `$` in a JVM/internal
/// name because it is also a legal source identifier.
pub(super) fn class_serial_name(ir: &IrFile, class_id: ClassId) -> KtString {
    class_serial_name_override(ir, class_id).unwrap_or_else(|| {
        ir.class_source_qualified_name(class_id)
            .expect("a serializable source classifier has a qualified declaration name")
    })
}

/// A class-level `@SerialName("…")`. An application whose value is not its `String` constant is a
/// frontend defect, not a reason to fall back to the qualified name.
fn class_serial_name_override(ir: &IrFile, class_id: ClassId) -> Option<KtString> {
    let serial_name = type_name(SERIAL_NAME_FQ);
    let application = ir.classes[class_id as usize]
        .applied_annotations
        .applications()
        .find(|application| application.internal == serial_name)?;
    match application.values.first() {
        Some((_, crate::ir::AnnoValue::Const(crate::ir::IrConst::String(value)))) => {
            Some(value.clone())
        }
        other => panic!("a class-level `@SerialName` carries its String value, not {other:?}"),
    }
}

pub(super) fn property_is_contextual(
    ctx: &PluginContext,
    ir: &IrFile,
    class_id: ClassId,
    property: &str,
) -> bool {
    if ctx.property_has_annotation(ir, class_id, property, type_name(CONTEXTUAL_FQ)) {
        return true;
    }
    ctx.property_canonical_type(ir, class_id, property)
        .is_some_and(|canonical| {
            ctx.file_annotation_mentions_canonical_type(
                ir,
                type_name(USE_CONTEXTUAL_SERIALIZATION_FQ),
                canonical,
            )
        })
}

/// Whether a TYPE is named by the file's `@UseContextualSerialization`. This is the ELEMENT form of
/// the property rule above: `List<FlexibleMap>` serializes its ELEMENTS contextually, so the
/// collection's element serializer is a `ContextualSerializer` even though no property of that type
/// exists.
pub(super) fn type_is_contextual(
    ctx: &PluginContext,
    ir: &IrFile,
    internal: crate::types::TypeName,
) -> bool {
    ctx.file_annotation_mentions_canonical_type(
        ir,
        type_name(USE_CONTEXTUAL_SERIALIZATION_FQ),
        internal,
    )
}

/// kotlinc hides the generated `$serializer` implementation from source resolution while keeping
/// it callable by compiler-generated code.
pub(super) fn generated_serializer_annotations() -> DeclarationAnnotations {
    DeclarationAnnotations::new(vec![RetainedAnnotation {
        retention: AnnotationRetention::Runtime,
        annotation: AppliedAnnotation {
            internal: type_name("kotlin/Deprecated"),
            values: vec![
                (
                    "message".to_string(),
                    AnnoValue::Const(IrConst::String(KtString::from(
                        "This synthesized declaration should not be used directly",
                    ))),
                ),
                (
                    "level".to_string(),
                    AnnoValue::Enum(type_name("kotlin/DeprecationLevel"), "HIDDEN".to_string()),
                ),
            ],
        },
    }])
}

#[cfg(test)]
mod tests {
    use super::write_self_annotations;
    use crate::types::type_name;

    #[test]
    fn write_self_has_exact_jvm_static_annotation() {
        let annotations = write_self_annotations();
        let applications = annotations.applications().collect::<Vec<_>>();
        assert_eq!(applications.len(), 1);
        assert_eq!(applications[0].internal, type_name("kotlin/jvm/JvmStatic"));
        assert!(applications[0].values.is_empty());
    }
}
