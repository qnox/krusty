use super::test_support::{checked_function_body_with_platform, jvm_stdlib_semantics};
use super::*;

/// Whether each checked range construction in `source`'s `range` ends an inlined call, in
/// construction order.
fn ranges_ending_inlined_calls(source: &str) -> Vec<bool> {
    let (body, _) = checked_function_body_with_platform(source, "range", jvm_stdlib_semantics());
    (0..body.expression_count())
        .filter_map(|raw| {
            let id = crate::fir::FirExprId::from_raw(raw as u32);
            match body.expr(id).expect("checked expression").kind {
                FirExprKind::Range {
                    ends_inlined_call, ..
                } => Some(ends_inlined_call),
                _ => None,
            }
        })
        .collect()
}

#[test]
fn an_inline_unsigned_range_operator_ends_an_inlined_call() {
    assert_eq!(
        ranges_ending_inlined_calls(
            "// WITH_STDLIB\nfun range(a: UInt, b: UInt): UIntRange = a..b\n"
        ),
        [true]
    );
    assert_eq!(
        ranges_ending_inlined_calls(
            "// WITH_STDLIB\nfun range(a: ULong, b: ULong): ULongRange = a..<b\n"
        ),
        [true]
    );
    assert_eq!(
        ranges_ending_inlined_calls(
            "// WITH_STDLIB\nfun range(a: UInt, b: UInt): UIntRange = a.rangeTo(b)\n"
        ),
        [true]
    );
}

/// The implicit-receiver member call converges on the explicit one's realization.
#[test]
fn an_implicit_receiver_unsigned_range_call_ends_an_inlined_call() {
    assert_eq!(
        ranges_ending_inlined_calls(
            "// WITH_STDLIB\nfun UInt.range(b: UInt): UIntRange = rangeTo(b)\n"
        ),
        [true]
    );
    assert_eq!(
        ranges_ending_inlined_calls(
            "// WITH_STDLIB\nfun UInt.range(b: UInt): UIntRange = rangeUntil(b)\n"
        ),
        [true]
    );
}

/// Membership through a user `contains` keeps the selected range construction as its receiver.
#[test]
fn a_range_kept_for_a_selected_contains_ends_an_inlined_call_when_its_operator_is_inline() {
    assert_eq!(
        ranges_ending_inlined_calls(
            "// WITH_STDLIB\n\
             operator fun UIntRange.contains(value: Long): Boolean = true\n\
             fun range(a: UInt, b: UInt, c: Long): Boolean = c in a..b\n"
        ),
        [true]
    );
    assert_eq!(
        ranges_ending_inlined_calls(
            "// WITH_STDLIB\n\
             operator fun IntRange.contains(value: String): Boolean = true\n\
             fun range(a: Int, b: Int, c: String): Boolean = c in a..b\n"
        ),
        [false]
    );
}

#[test]
fn a_builtin_range_operator_ends_no_inlined_call() {
    assert_eq!(
        ranges_ending_inlined_calls("// WITH_STDLIB\nfun range(a: Int, b: Int): IntRange = a..b\n"),
        [false]
    );
    assert_eq!(
        ranges_ending_inlined_calls(
            "// WITH_STDLIB\nfun range(a: Long, b: Long): LongRange = a.rangeTo(b)\n"
        ),
        [false]
    );
}
