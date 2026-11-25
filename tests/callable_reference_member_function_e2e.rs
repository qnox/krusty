//! A member function wins a callable reference over an accessor-derived property of the same
//! name. `Thread::isDaemon` is a function reference, as kotlinc builds it; the portable
//! `List<String>::isEmpty` case is the shared box regression.

use super::common;

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
