//! A member function wins a callable reference over an accessor-derived property of the same
//! name. `List<String>::isEmpty` and `Thread::isDaemon` are function references, as kotlinc builds
//! them, and a `suspend` function type converts the first one like any function reference.

use super::common;

#[test]
fn a_member_function_beats_a_same_named_accessor_property_in_a_reference() {
    const SOURCE: &str = r#"fun listEmpty(): (List<String>) -> Boolean = List<String>::isEmpty

fun threadDaemon(): (Thread) -> Boolean = Thread::isDaemon
"#;
    common::assert_classes_identical_to_kotlinc(
        "MemberFunctionReference",
        SOURCE,
        &[
            "MemberFunctionReferenceKt$listEmpty$1",
            "MemberFunctionReferenceKt$threadDaemon$1",
        ],
    );
}

#[test]
fn a_member_function_reference_converts_to_a_suspend_function_type() {
    const SOURCE: &str = r#"fun listEmpty(): suspend (List<String>) -> Boolean = List<String>::isEmpty

fun arrayListEmpty(): suspend (ArrayList<String>) -> Boolean = ArrayList<String>::isEmpty
"#;
    common::assert_classes_identical_to_kotlinc(
        "SuspendMemberFunctionReference",
        SOURCE,
        &[
            "SuspendMemberFunctionReferenceKt$listEmpty$1",
            "SuspendMemberFunctionReferenceKt$arrayListEmpty$1",
        ],
    );
}
