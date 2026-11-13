//! A type parameter whose upper bound is itself (`class Bar<B : B>`) must not send assignability
//! around its bound forever. kotlinc reports `CYCLIC_GENERIC_UPPER_BOUND` on the bound; the
//! common supertype of the member's `Foo<B>` result and an `Int` is then computed without it.
//! The cyclic-bound error itself is still a false accept, listed in the diagnostics lane.

use super::common;

#[test]
fn a_call_over_a_self_bounded_type_parameter_terminates() {
    let source = "\
interface Foo<F> {
    fun getSum(): F = TODO()
}

fun <S> select(vararg args: S): S = TODO()

class Bar<B : B> : Foo<B> {
    val v = select(
        getSum(),
        42
    )
}
";
    let diagnostics = common::front_end_diagnostics_with_stdlib(source);
    assert_eq!(diagnostics.len(), 0, "{diagnostics:?}");
}
