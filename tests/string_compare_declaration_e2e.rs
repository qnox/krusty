//! A language-level string comparison keeps the JVM declaration's physical call while a target
//! with its own strings realizes the authenticated comparison operation.

use super::common;

#[test]
fn string_comparison_keeps_the_selected_jvm_call_abi() {
    let source = "fun compare(a: String, b: String): Int = a.compareTo(b)\n";
    let classes = common::classes_against_kotlinc_module(&[("StringComparison.kt", source)]);
    assert_eq!(
        classes.reference.keys().collect::<Vec<_>>(),
        classes.krusty.keys().collect::<Vec<_>>(),
        "both compilers emit the same complete class inventory"
    );
    let (reference, actual) = classes.class_listing("StringComparisonKt");
    assert_eq!(
        actual, reference,
        "the selected JVM invocation and ABI stay exact"
    );
}

#[test]
fn a_platform_string_comparison_runs_the_whole_portable_program() {
    let source = include_str!("box_regressions/stringCompareTo.kt");
    common::expect_box_same_as_kotlinc(source, "StringComparisonProgram");
    common::expect_native_box(source, "StringComparisonProgram", "OK");
}
