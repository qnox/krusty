//! Explicit backing field declarations and their owner-visible read type.

use super::*;

#[test]
fn explicit_backing_field_requires_read_only_property() {
    let errors = check_with_detected_features(
        "// LANGUAGE: +ExplicitBackingFields\n\
         class Holder {\n\
             var value: Any field: String = \"value\"\n\
         }",
    );
    assert_eq!(
        errors,
        ["an explicit backing field requires a final, read-only property with default accessors"]
    );
}

#[test]
fn explicit_backing_field_type_must_refine_property_type() {
    let errors = check_with_detected_features(
        "// LANGUAGE: +ExplicitBackingFields\n\
         class Holder {\n\
             val value: String field: Any = \"value\"\n\
         }",
    );
    assert_eq!(
        errors,
        ["backing field type of 'value' is 'Any', which is not a subtype of its property type 'String'."]
    );
}

#[test]
fn explicit_backing_field_accepts_semantic_value_class_type() {
    let errors = check_with_annotation_fixtures(
        "// LANGUAGE: +ExplicitBackingFields\n\
         @kotlin.jvm.JvmInline value class Label(val text: String)\n\
         class Holder {\n\
             val value: Any field: Label = Label(\"value\")\n\
         }",
        true,
    )
    .0;
    assert!(errors.is_empty(), "{errors:?}");
}

#[test]
fn owner_reads_explicit_backing_field_at_its_narrower_type() {
    let errors = check_with_detected_features(
        "// LANGUAGE: +ExplicitBackingFields\n\
         interface Base { val value: Any }\n\
         class Holder : Base {\n\
             final override val value: Any field: String = \"OK\"\n\
             fun read(): String = accept(value)\n\
         }\n\
         fun accept(value: String): String = value\n",
    );
    assert!(errors.is_empty(), "{errors:?}");
}
