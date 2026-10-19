use super::test_support::{
    checked_function_body_with_platform, jvm_stdlib_semantics, root_expression,
};
use super::*;

/// Whether the checked range a one-expression function returns ends an inlined call.
fn range_ends_inlined_call(source: &str) -> bool {
    let (body, _) = checked_function_body_with_platform(source, "range", jvm_stdlib_semantics());
    let FirExprKind::Range {
        ends_inlined_call, ..
    } = body.expr(root_expression(&body)).expect("range").kind
    else {
        panic!("the selected range declaration publishes a checked range")
    };
    ends_inlined_call
}

#[test]
fn an_inline_unsigned_range_operator_ends_an_inlined_call() {
    assert!(range_ends_inlined_call(
        "// WITH_STDLIB\nfun range(a: UInt, b: UInt): UIntRange = a..b\n"
    ));
    assert!(range_ends_inlined_call(
        "// WITH_STDLIB\nfun range(a: ULong, b: ULong): ULongRange = a..<b\n"
    ));
    assert!(range_ends_inlined_call(
        "// WITH_STDLIB\nfun range(a: UInt, b: UInt): UIntRange = a.rangeTo(b)\n"
    ));
}

#[test]
fn a_builtin_range_operator_ends_no_inlined_call() {
    assert!(!range_ends_inlined_call(
        "// WITH_STDLIB\nfun range(a: Int, b: Int): IntRange = a..b\n"
    ));
    assert!(!range_ends_inlined_call(
        "// WITH_STDLIB\nfun range(a: Long, b: Long): LongRange = a.rangeTo(b)\n"
    ));
}
