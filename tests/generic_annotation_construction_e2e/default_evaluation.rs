//! Closed annotation defaults and ordinary calls retain distinct runtime schedules.

use super::common;

const PROVIDED_ENUM_DEFAULT: &str =
    include_str!("../fixtures/annotation_default_evaluation/DefaultEnumOrder.kt");
const DIRECT_ENUM_ARGUMENTS: &str =
    include_str!("../fixtures/annotation_default_evaluation/DirectEnumOrder.kt");
const OMITTED_ENUM_DEFAULT: &str =
    include_str!("../fixtures/annotation_default_evaluation/OmittedEnumOrder.kt");

// PLATFORM: compare complete JVM class artifacts, including source/debug and metadata attributes.
#[test]
fn provided_enum_default_arguments_match_complete_artifacts() {
    let classes =
        common::classes_against_kotlinc_module(&[("DefaultEnumOrder.kt", PROVIDED_ENUM_DEFAULT)]);
    assert_eq!(classes.differences(), Vec::<String>::new());
}

#[test]
fn provided_enum_default_arguments_follow_declaration_order() {
    let reference = common::kotlinc_box_result(PROVIDED_ENUM_DEFAULT);
    assert_eq!(reference, "DW:VALUE:VALUE");
    assert_eq!(
        common::expect_box_run_with_stdlib(PROVIDED_ENUM_DEFAULT, "DefaultEnumOrder"),
        reference
    );
}

// PLATFORM: compare complete JVM class artifacts for an ordinary written-order call.
#[test]
fn direct_enum_arguments_match_complete_artifacts() {
    let classes =
        common::classes_against_kotlinc_module(&[("DirectEnumOrder.kt", DIRECT_ENUM_ARGUMENTS)]);
    assert_eq!(classes.differences(), Vec::<String>::new());
}

#[test]
fn direct_enum_arguments_retain_written_order() {
    let reference = common::kotlinc_box_result(DIRECT_ENUM_ARGUMENTS);
    assert_eq!(reference, "WD:VALUE:VALUE");
    assert_eq!(
        common::expect_box_run_with_stdlib(DIRECT_ENUM_ARGUMENTS, "DirectEnumOrder"),
        reference
    );
}

// PLATFORM: compare complete JVM class artifacts, including the runtime default-mask constructor.
#[test]
fn omitted_enum_defaults_retain_complete_default_mask_artifacts() {
    let classes =
        common::classes_against_kotlinc_module(&[("OmittedEnumOrder.kt", OMITTED_ENUM_DEFAULT)]);
    assert_eq!(classes.differences(), Vec::<String>::new());
}

#[test]
fn omitted_enum_defaults_retain_runtime_default_evaluation() {
    let reference = common::kotlinc_box_result(OMITTED_ENUM_DEFAULT);
    assert_eq!(reference, "WD:VALUE:VALUE");
    assert_eq!(
        common::expect_box_run_with_stdlib(OMITTED_ENUM_DEFAULT, "OmittedEnumOrder"),
        reference
    );
}

#[test]
fn provided_declaration_default_has_the_reference_order_natively() {
    let reference = common::kotlinc_box_result(PROVIDED_ENUM_DEFAULT);
    assert_eq!(reference, "DW:VALUE:VALUE");
    common::expect_native_box(PROVIDED_ENUM_DEFAULT, "DefaultEnumOrder", &reference);
}

#[test]
fn ordinary_direct_arguments_keep_the_reference_order_natively() {
    let reference = common::kotlinc_box_result(DIRECT_ENUM_ARGUMENTS);
    assert_eq!(reference, "WD:VALUE:VALUE");
    common::expect_native_box(DIRECT_ENUM_ARGUMENTS, "DirectEnumOrder", &reference);
}

#[test]
fn omitted_arguments_keep_runtime_default_evaluation_natively() {
    let reference = common::kotlinc_box_result(OMITTED_ENUM_DEFAULT);
    assert_eq!(reference, "WD:VALUE:VALUE");
    common::expect_native_box(OMITTED_ENUM_DEFAULT, "OmittedEnumOrder", &reference);
}
