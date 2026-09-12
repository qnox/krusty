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

pub(super) fn custom_serializer_of(
    ctx: &PluginContext,
    ir: &IrFile,
    class_id: ClassId,
) -> Option<String> {
    ctx.class_annotation_class_literal_internal(ir, class_id, type_name(SERIALIZABLE_FQ))
}

pub(super) fn field_serializer_of(
    ctx: &PluginContext,
    ir: &IrFile,
    class_id: ClassId,
    property: &str,
) -> Option<String> {
    ctx.property_annotation_class_literal_internal(
        ir,
        class_id,
        property,
        type_name(SERIALIZABLE_FQ),
    )
}

pub(super) fn serial_name_of(
    ctx: &PluginContext,
    ir: &IrFile,
    class_id: ClassId,
    property: &str,
) -> Option<KtString> {
    ctx.property_annotation_const_string(ir, class_id, property, type_name(SERIAL_NAME_FQ))
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
