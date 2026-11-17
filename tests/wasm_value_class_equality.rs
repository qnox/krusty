//! Nominal value-class equality and a reference payload's own `equals`.
//!
//! The programs live in the shared box inventory. The JVM oracle runs the prepared source, and
//! each Wasm target runs that same program with the placeholder expanded for Wasm.

use super::{focused_wasm_lane_available, WasmLane};
use crate::box_lane::{Lane, Outcome};
use krusty_wasm::WasmTarget;

fn run(source: &str, stem: &str) {
    crate::box_reference_classes::checked_box(source, stem)
        .unwrap_or_else(|error| panic!("{error}"));
    for target in WasmTarget::ALL {
        let lane = WasmLane { target };
        assert!(
            focused_wasm_lane_available(&lane, stem),
            "the owned {stem} program requires its Wasm lane"
        );
        let prepared = krusty::conformance::prepare_test_source(source, lane.test_target());
        assert_eq!(
            lane.compile_and_run(
                &std::env::temp_dir(),
                stem,
                &[(format!("{stem}.kt"), prepared)],
                source,
            ),
            Outcome::Pass,
            "{stem} on {}",
            target.name(),
        );
    }
}

#[test]
fn distinct_value_classes_with_the_same_layout_are_unequal() {
    run(
        include_str!("box_regressions/DistinctValueClassBoxes.kt"),
        "DistinctValueClassBoxes",
    );
}

#[test]
fn a_value_class_compares_a_reference_payload_with_its_equals() {
    run(
        include_str!("box_regressions/ValueClassPayloadEquality.kt"),
        "ValueClassPayloadEquality",
    );
}

#[test]
fn nullable_unsigned_values_use_their_own_box_in_a_reference_array() {
    run(
        include_str!("box_regressions/unsignedValueBoxes.kt"),
        "unsignedValueBoxes",
    );
}

#[test]
fn a_value_class_is_its_box_in_a_reference_array() {
    run(
        include_str!("box_regressions/ValueClassReferenceArray.kt"),
        "ValueClassReferenceArray",
    );
}

#[test]
fn not_null_assertion_unboxes_every_primitive_box() {
    run(
        include_str!("box_regressions/NullableScalarNotNullAssert.kt"),
        "NullableScalarNotNullAssert",
    );
}

#[test]
fn unsigned_narrow_increment_wraps() {
    run(
        include_str!("box_regressions/UnsignedNarrowWraparound.kt"),
        "UnsignedNarrowWraparound",
    );
}

#[test]
fn unsigned_storage_read_sign_extends() {
    run(
        include_str!("box_regressions/UnsignedToSignedNarrowing.kt"),
        "UnsignedToSignedNarrowing",
    );
}

/// PLATFORM-SPECIFIC: both Wasm hosts execute the same whole portable graph, including when
/// the checked type-test file is emitted before the class declarations and when it is last.
#[test]
fn cross_file_nominal_box_tests_share_the_planned_class_identity_in_both_file_orders() {
    for (source, stem, expected_files) in [
        (
            include_str!("box_regressions/multimodule/crossFileNominalBoxTests.kt"),
            "CrossFileNominalBoxTests",
            3,
        ),
        (
            include_str!("box_regressions/multimodule/dependencyInterfaceAppliedAbi.kt"),
            "DependencyInterfaceAppliedAbiNominalBoxes",
            2,
        ),
    ] {
        crate::box_reference_classes::checked_box(source, stem)
            .expect("the complete nominal-box program passes the official JVM oracle");
        for target in WasmTarget::ALL {
            let lane = WasmLane { target };
            assert!(focused_wasm_lane_available(&lane, stem));
            let prepared = krusty::conformance::prepare_test_source(source, lane.test_target());
            let modules =
                krusty::conformance::split_modules(&prepared).expect("the whole file graph");
            let mut units = krusty::conformance::module_units(&modules);
            let consumer = units.last_mut().expect("the complete consumer module");
            assert_eq!(consumer.files.len(), expected_files);
            for reverse in [false, true] {
                if reverse {
                    units.last_mut().unwrap().files.reverse();
                }
                assert_eq!(
                    lane.compile_modules_and_run(&std::env::temp_dir(), stem, &units, source),
                    Outcome::Pass,
                    "{stem} on {} with reverse={reverse}",
                    target.name(),
                );
            }
        }
    }
}
