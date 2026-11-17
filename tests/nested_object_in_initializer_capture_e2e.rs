//! An anonymous object constructed in another anonymous object's property initializer captures a
//! value the outer object captures too. That construction runs in the outer object's constructor
//! code, so kotlinc passes the outer constructor's parameter on and gives the outer object no
//! field for the value. Both classes are byte-identical to kotlinc's.

use super::common;

const PROGRAM: &str = include_str!("box_regressions/nestedObjectInInitializerCapture.kt");

#[test]
fn a_nested_object_built_in_an_initializer_takes_the_constructor_parameter() {
    assert_eq!(
        common::expect_box_run_with_stdlib(PROGRAM, "NestedObjectInInitializerCapture"),
        "OK"
    );
    for class in [
        "NestedObjectInInitializerCaptureKt$wrap$1",
        "NestedObjectInInitializerCaptureKt$wrap$1$value$1",
    ] {
        common::byte_diff_against_kotlinc_cp(
            "NestedObjectInInitializerCapture",
            PROGRAM,
            class,
            &[common::stdlib_jar()],
        )
        .expect("reference kotlinc is provisioned")
        .unwrap_or_else(|diff| panic!("{class} differs from kotlinc: {diff}"));
    }
}
