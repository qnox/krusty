//! A `try` whose body cannot complete takes its value from a handler. When that handler's value is
//! a value class's carrier, the `try` is the carrier too: coercing it to the value class is no
//! unboxing. krusty read the `try` as an unknown reference and cast it to the box, which failed on
//! the carrier (`Failure` is no `Outcome`).

use super::common;

const SRC: &str = "@JvmInline value class Outcome<out T>(val value: Any?) {\n\
    \x20   companion object {\n\
    \x20       inline fun <T> failure(cause: Throwable): Outcome<T> = Outcome(Failure(cause))\n\
    \x20   }\n\
    }\n\
    class Failure(val cause: Throwable)\n\
    fun fail(): Nothing = throw IllegalStateException()\n\
    inline fun <R> attempt(block: () -> R): Outcome<R> =\n\
    \x20   try { Outcome(block()) } catch (e: Throwable) { Outcome.failure(e) }\n\
    fun failed(): Boolean = attempt { fail() }.value is Failure\n\
    fun box(): String = if (failed()) \"OK\" else \"fail\"\n";

#[test]
fn a_try_taking_its_value_class_from_a_handler_is_the_carrier() {
    assert_eq!(common::expect_box_run_with_stdlib(SRC, "Main"), "OK");
}
