//! Parameterless `startCoroutine` delivers its result, `Continuation` is an object, and the
//! ordinary call evaluates the receiver before the completion. The factory object is that
//! interface, and a completion that is a different object is refused before it is emitted.
use super::common;

const COMPLETION: &str = include_str!("box_regressions/parameterlessStartCoroutineCompletion.kt");
const VALUE: &str = include_str!("box_regressions/continuationFactoryValue.kt");
const ORDER: &str = include_str!("box_regressions/startCoroutineOperandOrder.kt");
const HASH: &str = include_str!("box_regressions/continuationIdentityHash.kt");
const IDENTITY: &str = include_str!("box_regressions/continuationIdentity.kt");
// PLATFORM-SPECIFIC: the JVM runs this through the stdlib. Native declines it: the completion is
// a different object from the one the factory allocates, and emitting that call would trap.
const FOREIGN: &str = include_str!("foreignStartCoroutineCompletion.kt");
const FOREIGN_TYPED: &str = include_str!("foreignContinuationTypedCompletion.kt");
const ACROSS_FILES: &str = include_str!("box_regressions/continuationAcrossFiles.kt");

#[test]
fn a_parameterless_coroutine_delivers_its_completion() {
    common::expect_box_same_as_kotlinc(COMPLETION, "ParameterlessStartCoroutineCompletion");
    common::expect_native_box(COMPLETION, "ParameterlessStartCoroutineCompletion", "OK");
}

#[test]
fn a_continuation_value_is_not_null() {
    common::expect_box_same_as_kotlinc(VALUE, "ContinuationFactoryValue");
    common::expect_native_box(VALUE, "ContinuationFactoryValue", "OK");
}

#[test]
fn start_coroutine_evaluates_the_receiver_first() {
    common::expect_box_same_as_kotlinc(ORDER, "StartCoroutineOperandOrder");
    common::expect_native_box(ORDER, "StartCoroutineOperandOrder", "OK");
}

#[test]
fn a_continuation_hash_code_is_stable() {
    common::expect_box_same_as_kotlinc(HASH, "ContinuationIdentityHash");
    common::expect_native_box(HASH, "ContinuationIdentityHash", "OK");
}

#[test]
fn a_continuation_is_its_interface_and_keeps_its_context() {
    common::expect_box_same_as_kotlinc(IDENTITY, "ContinuationIdentity");
    common::expect_native_box(IDENTITY, "ContinuationIdentity", "OK");
}

#[test]
fn a_foreign_completion_declines() {
    common::expect_box_same_as_kotlinc(FOREIGN, "ForeignStartCoroutineCompletion");
    common::expect_native_decline(
        FOREIGN,
        "ForeignStartCoroutineCompletion",
        "a startCoroutine completion that is not the factory continuation",
    );
}

#[test]
fn a_continuation_typed_foreign_completion_is_not_called() {
    assert_eq!(
        common::kotlinc_box_result(FOREIGN_TYPED),
        "FAIL",
        "ForeignContinuationTypedCompletion: kotlinc calls the foreign resume"
    );
    assert_eq!(
        common::expect_box_run_with_stdlib_diverging(
            FOREIGN_TYPED,
            "ForeignContinuationTypedCompletion",
            "OK",
        ),
        "FAIL"
    );
}

#[test]
fn a_continuation_from_another_file_is_its_interface() {
    let files = krusty::conformance::split_files(ACROSS_FILES).0;
    let sources: Vec<(&str, &str)> = files
        .iter()
        .map(|(name, source)| (name.as_str(), source.as_str()))
        .collect();
    let named: Vec<(String, String)> = files
        .iter()
        .map(|(name, source)| (format!("{name}.kt"), source.clone()))
        .collect();
    let kotlinc: Vec<(&str, &str)> = named
        .iter()
        .map(|(name, source)| (name.as_str(), source.as_str()))
        .collect();
    assert_eq!(
        common::kotlinc_box_files_result(&kotlinc, "MainKt"),
        "OK",
        "ContinuationAcrossFiles"
    );
    common::expect_box_ok_files_with_stdlib(&sources, "ContinuationAcrossFiles");
    common::expect_native_sources(&sources, "OK");
}
