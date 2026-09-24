//! Unboxing a reference into a primitive follows kotlinc's `StackValue.coerce`.
//!
//! A wrapper already on the stack unboxes through its own accessor (`as Int` leaves an `Integer`,
//! so `Integer.intValue` follows with no second `checkcast`). Any other reference reaches a number
//! through `java/lang/Number` (`checkcast Number; Number.intValue`), and `Boolean` and `Char`
//! through their own wrappers. An `as` cast guards against `null` only when its operand's type
//! admits `null`, and a variable operand is read again after the guard instead of duplicated.
//!
//! Each shape below is byte-identical to the facade kotlinc 2.4.10 writes for the same source.

use super::common;

#[test]
fn unboxing_and_casts_match_kotlinc_byte_for_byte() {
    let source = r#"
        package u
        fun first(l: List<Int>): Int = l[0]
        fun cast(a: Any): Int = a as Int
        fun str(a: Any?): String = a as String
        fun flag(m: Map<String, Boolean>): Boolean = m.getValue("k")
        fun ch(l: List<Char>): Char = l[0]
        fun wide(l: List<Long>): Long = l[0]
        fun arr(a: Array<Int>): Int = a[0]
        fun call(f: (Int) -> Double): Double = f(1)
        fun box(): String {
            val ok = first(listOf(1)) == 1 && cast(2) == 2 && str("s") == "s" &&
                flag(mapOf("k" to true)) && ch(listOf('c')) == 'c' && wide(listOf(3L)) == 3L &&
                arr(arrayOf(4)) == 4 && call { it.toDouble() } == 1.0
            return if (ok) "OK" else "fail"
        }
    "#;
    let result = common::compile_and_run_with_stdlib(source, "Unboxing");
    assert_eq!(result.as_deref(), Some("OK"));
    let checked = r#"
        package u
        fun first(l: List<Int>): Int = l[0]
        fun cast(a: Any): Int = a as Int
        fun str(a: Any?): String = a as String
        fun flag(m: Map<String, Boolean>): Boolean = m.getValue("k")
        fun ch(l: List<Char>): Char = l[0]
        fun wide(l: List<Long>): Long = l[0]
        fun arr(a: Array<Int>): Int = a[0]
        fun call(f: (Int) -> Double): Double = f(1)
    "#;
    match common::byte_diff_against_kotlinc_cp(
        "Unboxing",
        checked,
        "u/UnboxingKt",
        &[common::stdlib_jar()],
    ) {
        Some(Ok(())) | None => {}
        Some(Err(difference)) => panic!("unboxing facade differs from kotlinc:\n{difference}"),
    }
}

#[test]
fn a_failed_cast_of_a_null_variable_names_the_target_type() {
    let source = r#"
        fun str(a: Any?): String = a as String
        fun box(): String = try {
            str(null)
            "fail: no exception"
        } catch (e: NullPointerException) {
            if (e.message == "null cannot be cast to non-null type kotlin.String") "OK" else e.message!!
        }
    "#;
    assert_eq!(
        common::compile_and_run_with_stdlib(source, "NullCast").as_deref(),
        Some("OK")
    );
}
