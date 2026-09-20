//! `kotlin.math` through krusty's own code generator.
//!
//! These are facade declarations — `kotlin/math/MathKt__MathJVMKt` — so they reach the backend by
//! the path that looks a name up in the runtime table. `abs` is deliberately not in that table:
//! the table crosses every operand as a REFERENCE, which for a `Double` means boxing the very
//! value the question is about. It is realized in place instead, beside `float_predicate`, which
//! sits there for the same reason.

use super::common::expect_native_box;

#[test]
fn abs_answers_at_each_width_the_overload_was_selected_for() {
    // Kotlin declares `abs` four times, one per signed arithmetic type, and three of the answers
    // below are the ones an implementation gets wrong by guessing:
    //
    //   * `abs(Int.MIN_VALUE)` is `Int.MIN_VALUE`, and `abs(Long.MIN_VALUE)` likewise. The
    //     magnitude has no representation at that width, so two's-complement negation answers the
    //     input again. A backend that treated the wrap as an overflow to guard would answer
    //     something else.
    //   * `abs(-0.0)` is `+0.0`, which only the reciprocal can see: `1.0 / +0.0` is `Infinity`
    //     where `1.0 / -0.0` is `-Infinity`. A `< 0` test leaves `-0.0` alone, since `-0.0 < 0` is
    //     false, and answers `-Infinity` here.
    //   * `abs(NaN)` is `NaN`. Under a comparison it is neither less nor greater than zero, so it
    //     takes the identity arm — which happens to be right, but for a reason that does not hold
    //     for `-0.0` beside it. Clearing the sign bit is one rule for both.
    //
    // Every expectation is kotlinc's, taken by compiling and running this exact source under it.
    expect_native_box(
        "import kotlin.math.abs\n\
         fun box(): String =\n\
         \x20   \"\" + abs(-7) + \",\" + abs(7) + \",\" + abs(0) + \",\" +\n\
         \x20   abs(-7L) + \",\" + abs(7L) + \",\" +\n\
         \x20   abs(Int.MIN_VALUE) + \",\" + abs(Long.MIN_VALUE) + \",\" +\n\
         \x20   abs(-2.5) + \",\" + abs(2.5) + \",\" +\n\
         \x20   abs(-2.5f) + \",\" + abs(2.5f) + \",\" +\n\
         \x20   (1.0 / abs(-0.0)) + \",\" + (1.0f / abs(-0.0f)) + \",\" +\n\
         \x20   abs(Double.NaN).isNaN() + \",\" + abs(Float.NaN).isNaN() + \",\" +\n\
         \x20   abs(Double.NEGATIVE_INFINITY) + \",\" + abs(Float.NEGATIVE_INFINITY)\n",
        "AbsAtEveryWidth",
        "7,7,0,7,7,-2147483648,-9223372036854775808,2.5,2.5,2.5,2.5,Infinity,Infinity,true,true,Infinity,Infinity",
    );
}
