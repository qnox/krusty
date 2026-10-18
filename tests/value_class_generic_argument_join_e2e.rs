//! A value class reaching a type-parameter slot through a join (a safe call or an `if`) is boxed
//! once, after the join, as kotlinc does.

use super::common;

const SOURCE: &str = "@JvmInline
value class Wid(val value: String)
@JvmInline
value class Meter(val value: Int)
class W(val id: Wid, val length: Meter)
fun find(ok: Boolean): W? = if (ok) W(Wid(\"w\"), Meter(3)) else null
fun <T> same(actual: T, expected: T): String = if (actual == expected) \"\" else \"[$actual != $expected]\"
infix fun <T> T.sameAs(expected: T): String = same(this, expected)
fun safeArgument(ok: Boolean): String = same(find(ok)?.id, if (ok) Wid(\"w\") else null)
fun safeReceiver(): String = find(true)?.id sameAs Wid(\"w\")
fun parenthesizedReceiver(): String = (find(true)?.id).sameAs(Wid(\"w\"))
fun storedFirst(): String {
    val id: Wid? = find(true)?.id
    return same(id, Wid(\"w\"))
}
fun primitiveSafeArgument(ok: Boolean): String = same(find(ok)?.length, if (ok) Meter(3) else null)
fun branchArgument(ok: Boolean): String = same(if (ok) Wid(\"w\") else Wid(\"x\"), Wid(\"w\"))
fun nullableBranchArgument(ok: Boolean, id: Wid?): String = same(if (ok) id else Wid(\"x\"), Wid(\"w\"))
fun box(): String {
    val failures = safeArgument(true) + safeArgument(false) + safeReceiver() +
        parenthesizedReceiver() + storedFirst() + primitiveSafeArgument(true) +
        primitiveSafeArgument(false) + branchArgument(true) + nullableBranchArgument(true, Wid(\"w\"))
    return if (failures.isEmpty()) \"OK\" else failures
}
";

#[test]
fn a_joined_value_class_argument_to_a_type_parameter_is_boxed_after_the_join() {
    assert_eq!(
        common::compile_and_run_with_stdlib(SOURCE, "Join").expect("compiles and runs"),
        "OK"
    );
}

#[test]
fn a_joined_value_class_argument_to_a_type_parameter_matches_kotlinc_bytes() {
    let classes = common::classes_against_kotlinc_module(&[("Join.kt", SOURCE)]);
    let differences = classes.differences();
    assert!(
        differences.is_empty(),
        "classes differ from kotlinc's:\n\n{}",
        differences.join("\n\n")
    );
}
