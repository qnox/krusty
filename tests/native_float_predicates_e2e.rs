//! `isNaN`, `isInfinite` and `isFinite` reached by their MEMBER spelling.
//!
//! Kotlin declares each of the three twice: as a member of the primitive (`Double.isNaN()`) and as
//! an extension on it in the numbers facade. Which spelling reaches a backend is the symbol
//! provider's choice, not the program's.
//!
//! `float_predicate` read only the facade one, because it gated on `facade_package`, which answers
//! `None` for an owner that does not end in `Kt`. An owner of `kotlin/Double` therefore fell
//! through and the call declined by name — 7 cases in the corpus's own `codegen/box/fp/`.
//!
//! `native_codegen_e2e::what_kotlin_asks_of_a_value_directly` covers the same three questions and
//! passed throughout, because the shapes it uses resolve to the facade. That is exactly why this
//! went unnoticed: the coverage existed and exercised the other spelling. Every program here
//! declined before the recognizer read both, so each is written in the shape that produces the
//! member spelling rather than in the one already covered.
//!
//! Expectations are kotlinc's, taken by compiling and running the same predicates under it.

use super::common::expect_native_box;

/// A local, a parameter, a computed value and a named constant — the receivers the corpus uses
/// (`x.isNaN()` over a remainder or a division, `Double.NaN.isNaN()`).
#[test]
fn a_double_answers_the_three_predicates_under_its_member_spelling() {
    expect_native_box(
        "fun viaParam(v: Double): Boolean = v.isNaN()\n\
         fun box(): String {\n\
         \x20   val nan: Double = 0.0 / 0.0\n\
         \x20   val inf: Double = 1.0 / 0.0\n\
         \x20   if (!nan.isNaN()) return \"fail local isNaN\"\n\
         \x20   if (!viaParam(0.0 / 0.0)) return \"fail param isNaN\"\n\
         \x20   if (viaParam(1.0)) return \"fail param not NaN\"\n\
         \x20   if (!(5.0 % 0.0).isNaN()) return \"fail remainder isNaN\"\n\
         \x20   if (!Double.NaN.isNaN()) return \"fail Double.NaN\"\n\
         \x20   if (!inf.isInfinite()) return \"fail isInfinite\"\n\
         \x20   if (nan.isInfinite()) return \"fail NaN is not infinite\"\n\
         \x20   if (inf.isFinite()) return \"fail infinity is not finite\"\n\
         \x20   if (nan.isFinite()) return \"fail NaN is not finite\"\n\
         \x20   if (!1.0.isFinite()) return \"fail finite\"\n\
         \x20   return \"OK\"\n\
         }\n",
        "DoublePredicates",
        "OK",
    );
}

/// The same three on `Float`, whose member spelling has its own owner (`kotlin/Float`).
///
/// Separate because a recognizer naming only `kotlin/Double` would pass the test above and leave
/// every `Float` receiver declining.
#[test]
fn a_float_answers_the_three_predicates_under_its_member_spelling() {
    expect_native_box(
        "fun viaParam(v: Float): Boolean = v.isNaN()\n\
         fun box(): String {\n\
         \x20   val nan: Float = 0.0f / 0.0f\n\
         \x20   val inf: Float = 1.0f / 0.0f\n\
         \x20   if (!nan.isNaN()) return \"fail local isNaN\"\n\
         \x20   if (!viaParam(0.0f / 0.0f)) return \"fail param isNaN\"\n\
         \x20   if (viaParam(1.0f)) return \"fail param not NaN\"\n\
         \x20   if (!Float.NaN.isNaN()) return \"fail Float.NaN\"\n\
         \x20   if (!inf.isInfinite()) return \"fail isInfinite\"\n\
         \x20   if (inf.isFinite()) return \"fail infinity is not finite\"\n\
         \x20   if (!1.0f.isFinite()) return \"fail finite\"\n\
         \x20   return \"OK\"\n\
         }\n",
        "FloatPredicates",
        "OK",
    );
}
