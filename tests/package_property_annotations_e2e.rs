//! PLATFORM: JVM property annotation carriers and their Kotlin metadata signatures.

use super::common;

#[test]
fn package_property_annotations_match_every_reference_class() {
    let source = include_str!("fixtures/package_property_annotations/Properties.kt");
    let classes = common::classes_against_kotlinc_module(&[("Properties.kt", source)]);
    assert_eq!(classes.differences(), Vec::<String>::new());
}

#[test]
fn parameterized_property_markers_report_complete_method_clashes() {
    common::assert_error_blocks_match_kotlinc(
        &[(
            "MarkerClashes.kt",
            "package store\n\
             @Target(AnnotationTarget.PROPERTY) annotation class Mark\n\
             @Mark val String.value: Int get() = length\n\
             fun `getValue$annotations`(value: String) {}\n\
             context(prefix: String)\n\
             @Mark val contextual: Int get() = prefix.length\n\
             fun `getContextual$annotations`(prefix: String) {}\n",
        )],
        &[],
    );
}

#[test]
fn mixed_marker_and_delegate_clashes_preserve_every_source_diagnostic() {
    for (name, source) in [
        (
            "MarkerThenDelegateClashes.kt",
            include_str!("fixtures/package_property_annotations/MarkerThenDelegateClashes.kt"),
        ),
        (
            "DelegateThenMarkerClashes.kt",
            include_str!("fixtures/package_property_annotations/DelegateThenMarkerClashes.kt"),
        ),
    ] {
        common::assert_error_blocks_match_kotlinc(&[(name, source)], &[]);
    }
}
