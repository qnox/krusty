//! Unboxing a reference into a primitive follows kotlinc's `StackValue.coerce`.
//!
//! A wrapper already on the stack unboxes through its own accessor (`as Int` leaves an `Integer`,
//! so `Integer.intValue` follows with no second `checkcast`). Any other reference reaches a number
//! through `java/lang/Number` (`checkcast Number; Number.intValue`), and `Boolean` and `Char`
//! through their own wrappers.
//!
//! An `as` cast to a non-null type guards its operand with `Intrinsics.checkNotNull` unless
//! kotlinc's nullability analysis proves the operand non-null: a constant, a `new`, an asserted
//! parameter, a value that passed `!!` or another cast, or a local stored only from such values. A
//! property read, a call result, `this` and a private function's unasserted parameter keep the guard,
//! whatever their declared type. A parameter or `val` is read again after the guard instead of
//! duplicated. The `checkcast` stays unless the operand already has exactly the target's
//! JVM type, so `as Any` from a narrower type still writes `checkcast java/lang/Object`.
//!
//! Each shape below is byte-identical to the facade kotlinc 2.4.10 writes for the same source.

use super::common;

#[test]
fn unboxing_and_casts_match_kotlinc_byte_for_byte() {
    let source = r#"
        package u
        interface Payload<T> { fun value(): T }
        class Stored<T>(private val item: T) : Payload<T> { override fun value(): T = item }
        fun first(v: Payload<Int>): Int = v.value()
        fun cast(a: Any): Int = a as Int
        fun str(a: Any?): String = a as String
        fun flag(v: Payload<Boolean>): Boolean = v.value()
        fun ch(v: Payload<Char>): Char = v.value()
        fun wide(v: Payload<Long>): Long = v.value()
        fun arr(a: Array<Int>): Int = a[0]
        fun call(f: (Int) -> Double): Double = f(1)
        fun box(): String {
            val ok = first(Stored(1)) == 1 && cast(2) == 2 && str("s") == "s" &&
                flag(Stored(true)) && ch(Stored('c')) == 'c' && wide(Stored(3L)) == 3L &&
                arr(arrayOf(4)) == 4 && call { it.toDouble() } == 1.0
            return if (ok) "OK" else "fail"
        }
    "#;
    let result = common::compile_and_run_with_stdlib(source, "Unboxing");
    assert_eq!(result.as_deref(), Some("OK"));
    let checked = r#"
        package u
        interface Payload<T> { fun value(): T }
        fun first(v: Payload<Int>): Int = v.value()
        fun cast(a: Any): Int = a as Int
        fun str(a: Any?): String = a as String
        fun flag(v: Payload<Boolean>): Boolean = v.value()
        fun ch(v: Payload<Char>): Char = v.value()
        fun wide(v: Payload<Long>): Long = v.value()
        fun arr(a: Array<Int>): Int = a[0]
        fun call(f: (Int) -> Double): Double = f(1)
        abstract class A { abstract val x: Any }
        fun property(a: A) = a.x as String
        fun parameter(a: Any) = a as String
        private fun unasserted(a: Any) = a as String
        fun local(a: A): String { val v = a.x; return v as String }
        class P
        fun fresh(): Any { val v = P(); return v as Any }
        fun asserted(a: A?) = a!! as Any
        fun twice(a: A) = (a.x as CharSequence) as String
        class Envelope<A, B>
        fun generic(a: Any?) = a as Envelope<String, Envelope<Int?, *>>
        fun projected(a: Any?) = a as Envelope<out String, *>
        fun <T : Any> parameterized(a: Any?) = a as T
        fun deep(a: Any): String {
            val a1 = a; val a2 = a1; val a3 = a2; val a4 = a3; val a5 = a4
            val a6 = a5; val a7 = a6; val a8 = a7; val a9 = a8
            return a9 as String
        }
        fun merged(flag: Boolean): String {
            var value: Any = "initial"
            if (flag) value = "left" else value = "right"
            return value as String
        }
        fun useUnasserted() = unasserted("")
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
fn a_cast_of_a_non_null_property_read_before_initialization_throws() {
    let source = r#"
        abstract class A {
            abstract val x: Any
            init { castX(this) }
        }
        class B : A() { override val x: Any = "abc" }
        fun castX(a: A) { a.x as String }
        fun box(): String = try {
            B()
            "fail: no exception"
        } catch (e: NullPointerException) {
            "OK"
        }
    "#;
    assert_eq!(
        common::compile_and_run_with_stdlib(source, "EarlyRead").as_deref(),
        Some("OK")
    );
}

#[test]
fn failed_null_casts_name_primitive_and_qualified_type_parameter_targets() {
    let source = r#"
        fun int(a: Any?): Int = a as Int
        fun <T : Any> generic(a: Any?): T = a as T
        fun box(): String = try {
            int(null)
            "fail: primitive cast did not throw"
        } catch (e: NullPointerException) {
            if (e.message != "null cannot be cast to non-null type kotlin.Int") e.message!! else
                try {
                    generic<String>(null)
                    "fail: type-parameter cast did not throw"
                } catch (nested: NullPointerException) {
                    if (nested.message == "null cannot be cast to non-null type T of NullCastKt.generic")
                        "OK"
                    else nested.message!!
                }
        }
    "#;
    assert_eq!(
        common::compile_and_run_with_stdlib(source, "NullCast").as_deref(),
        Some("OK")
    );
}

/// A local's StackMapTable type is the type its store leaves in the slot. An initializer is cast to
/// the declared type when that type differs and is not `Object`, so `val g: Greeter = Ann()` records
/// `Greeter`, as kotlinc does. An assignment, or an `Any` local, keeps the exact stored class.
#[test]
fn a_local_frame_type_follows_the_cast_its_initializer_takes() {
    let source = r#"
        package f
        interface Greeter { fun name(): String }
        class Ann : Greeter { override fun name(): String = "Ann" }
        fun declared(): String {
            val g: Greeter = Ann()
            if (g.name() != "Ann") return "f1"
            return "OK"
        }
        fun erased(): String {
            val g: Any = Ann()
            if (g.hashCode() == 0) return "f2"
            return "OK"
        }
        fun box(): String {
            if (declared() != "OK") return "fail"
            return erased()
        }
    "#;
    assert_eq!(
        common::compile_and_run_with_stdlib(source, "LocalFrame").as_deref(),
        Some("OK")
    );
    match common::byte_diff_against_kotlinc_cp(
        "LocalFrame",
        source,
        "f/LocalFrameKt",
        &[common::stdlib_jar()],
    ) {
        Some(Ok(())) | None => {}
        Some(Err(difference)) => panic!("local frame facade differs from kotlinc:\n{difference}"),
    }
}
