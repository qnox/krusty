//! A generated structural `equals` is an ordinary override of `Any.equals`, not a compiler-known
//! equality bound: proving `a == C(...)` does not narrow `a` to `C`. kotlinc keeps `a` at its
//! declared type, so a member of `C` read through it is unresolved and a further comparison with an
//! unrelated type stays legal.

use super::common;

/// After a comparison with a data class or a value class, the other operand is still an `Any`.
#[test]
fn generated_equals_does_not_narrow_the_other_operand() {
    let src = "data class Point(val x: Int)
@JvmInline value class Meters(val amount: Int)
fun point(a: Any): Int {
    if (a != Point(1)) return 0
    return a.x
}
fun reversed(a: Any): Int {
    if (Point(1) != a) return 0
    return a.x
}
fun meters(a: Any): Int {
    if (a != Meters(1)) return 0
    return a.amount
}
";
    common::assert_errors_match_kotlinc(&[("main.kt", src)], &[]);
}

/// A later comparison of that operand with an unrelated type is not an incompatible equality.
#[test]
fn a_compared_operand_may_still_be_compared_with_another_type() {
    common::expect_box_ok_with_stdlib(
        "data class Point(val x: Int)
@JvmInline value class Meters(val amount: Int)
fun box(): String {
    val a: Any = Point(1)
    if (a != Point(1)) return \"point\"
    if (a == 1) return \"point and Int\"
    val b: Any = Meters(1)
    if (b != Meters(1)) return \"meters\"
    if (b == 1) return \"meters and Int\"
    return \"OK\"
}
",
        "GeneratedEqualsKeepsDeclaredType",
    );
}

/// A dependency's data class generates the same `equals`: its synthesized metadata `equals` names
/// no equality bound, so a comparison with `Pair` leaves the other operand an `Any` too.
#[test]
fn a_dependency_data_class_equals_does_not_narrow_the_other_operand() {
    let src = "fun pair(a: Any): Int {
    if (a != Pair(1, 2)) return 0
    return a.first as Int
}
";
    common::assert_errors_match_kotlinc(&[("main.kt", src)], &[]);
}
