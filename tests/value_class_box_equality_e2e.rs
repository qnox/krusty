//! JVM and Native oracles for the shared value-class programs.
//!
//! Equality is checked on both backends. Nullable reference-array stores are checked on the JVM,
//! where an element is the box or `null`.

use super::common;

fn prepared(source: &str, target: krusty::conformance::TestTarget) -> String {
    krusty::conformance::prepare_test_source(source, target)
}

fn oracles(source: &str, stem: &str) {
    common::expect_box_same_as_kotlinc(
        &prepared(source, krusty::conformance::TestTarget::Jvm),
        stem,
    );
    common::expect_native_box(
        &prepared(source, krusty::conformance::TestTarget::Native),
        stem,
        "OK",
    );
}

#[test]
fn distinct_value_classes_with_the_same_layout_are_unequal() {
    oracles(
        include_str!("box_regressions/DistinctValueClassBoxes.kt"),
        "DistinctValueClassBoxes",
    );
}

#[test]
fn a_value_class_compares_a_reference_payload_with_its_equals() {
    oracles(
        include_str!("box_regressions/ValueClassPayloadEquality.kt"),
        "ValueClassPayloadEquality",
    );
}

#[test]
fn nullable_unsigned_reference_arrays_store_boxes() {
    common::expect_box_same_as_kotlinc(
        include_str!("box_regressions/unsignedValueBoxes.kt"),
        "unsignedValueBoxes",
    );
}

#[test]
fn nullable_value_class_reference_arrays_store_boxes() {
    common::expect_box_same_as_kotlinc(
        &prepared(
            include_str!("box_regressions/ValueClassReferenceArray.kt"),
            krusty::conformance::TestTarget::Jvm,
        ),
        "ValueClassReferenceArray",
    );
}
