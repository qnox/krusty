//! A member function wins a callable reference over an accessor-derived property of the same
//! name. `Thread::isDaemon` is a function reference, as kotlinc builds it; the portable
//! `List<String>::isEmpty` case is the shared box regression. The synthetic property ranks after
//! the member functions, so it is selected only when no member function fits the expected type.

use super::common;
use super::diagnostics_parity_support::{errors, ObservedError};

#[test]
fn a_member_function_beats_a_same_named_accessor_property_in_a_reference() {
    const SOURCE: &str = "fun threadDaemon(): (Thread) -> Boolean = Thread::isDaemon\n";
    common::assert_classes_identical_to_kotlinc(
        "MemberFunctionReference",
        SOURCE,
        &["MemberFunctionReferenceKt$threadDaemon$1"],
    );
}

/// The portable program lives once in `tests/box_regressions/`, which every backend runs; here
/// its reference classes are compared with kotlinc's.
#[test]
fn a_member_function_reference_converts_to_a_suspend_function_type() {
    const SOURCE: &str = include_str!("box_regressions/suspendMemberFunctionReference.kt");
    common::assert_classes_identical_to_kotlinc(
        "SuspendMemberFunctionReference",
        SOURCE,
        &[
            "SuspendMemberFunctionReferenceKt$plainListEmpty$1",
            "SuspendMemberFunctionReferenceKt$listEmpty$1",
            "SuspendMemberFunctionReferenceKt$arrayListEmpty$1",
        ],
    );
}

/// No member function fits an expected `KProperty1`, so the reference selects the synthetic
/// property, which kotlinc rejects without `ReferencesToSyntheticJavaProperties`.
#[test]
fn an_expected_property_type_selects_the_unsupported_synthetic_property() {
    const SOURCE: &str = "import kotlin.reflect.KProperty1\n\
                          \n\
                          val p: KProperty1<Thread, Boolean> = Thread::isDaemon\n";
    let result = common::compiler_diagnostics(&[("SyntheticPropertyReference.kt", SOURCE)], &[]);
    let expected = vec![ObservedError {
        file: "SyntheticPropertyReference.kt".to_string(),
        line: 3,
        column: 46,
        message: "the feature \"references to synthetic java properties\" is unsupported."
            .to_string(),
    }];
    assert_eq!(
        errors(&result.reference_stderr),
        expected,
        "kotlinc's diagnostics"
    );
    let mut krusty_errors = errors(&result.krusty_stderr);
    krusty_errors.extend(errors(&result.krusty_stdout));
    assert_eq!(krusty_errors, expected, "krusty's diagnostics");
    assert_ne!(result.krusty_code, 0, "krusty accepted the reference");
}

/// A bound reference follows the same rung order: an expected `KProperty0` admits no member
/// function, so `thread::isDaemon` selects the unsupported synthetic property.
#[test]
fn an_expected_bound_property_type_selects_the_unsupported_synthetic_property() {
    const SOURCE: &str = "import kotlin.reflect.KProperty0\n\
                          \n\
                          fun daemon(thread: Thread) {\n\
                          \x20   val p: KProperty0<Boolean> = thread::isDaemon\n\
                          }\n";
    let result =
        common::compiler_diagnostics(&[("BoundSyntheticPropertyReference.kt", SOURCE)], &[]);
    let expected = vec![ObservedError {
        file: "BoundSyntheticPropertyReference.kt".to_string(),
        line: 4,
        column: 42,
        message: "the feature \"references to synthetic java properties\" is unsupported."
            .to_string(),
    }];
    assert_eq!(
        errors(&result.reference_stderr),
        expected,
        "kotlinc's diagnostics"
    );
    let mut krusty_errors = errors(&result.krusty_stderr);
    krusty_errors.extend(errors(&result.krusty_stdout));
    assert_eq!(krusty_errors, expected, "krusty's diagnostics");
    assert_ne!(result.krusty_code, 0, "krusty accepted the reference");
}
