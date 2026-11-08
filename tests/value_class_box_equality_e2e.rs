//! The JVM and Native oracles for the shared value-class equality programs.

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
