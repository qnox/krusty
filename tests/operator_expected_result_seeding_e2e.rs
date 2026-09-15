//! A binary operator's type parameter is seeded from the call's expected result.
//!
//! `fun pick(): List<P> = listOf(A()) + listOf(B())` was rejected:
//!
//! ```text
//! error: argument type mismatch: actual type is 'List<B>', but 'Iterable<A>' was expected.
//! ```
//!
//! `Iterable<T>.plus(Iterable<T>): List<T>` shares one `T` between the receiver, the argument and the
//! result. krusty fixed it from the RECEIVER alone — the receiver call had already been committed to
//! `List<A>` — so the argument was judged against `Iterable<A>`. Kotlin solves receiver, argument and
//! expected result together: the declared `List<P>` fixes `T = P`, and both operands then fit.
//!
//! Every spelling that pins `T` before the operator runs already worked — an explicit type argument
//! on the receiver call, or a declared `val` receiver — which is what isolates the expected result as
//! the missing input rather than the operands.

use super::common;

/// Run one fixture under BOTH compilers and require the same `box()` value.
fn both_compilers_box(main: &str, stem: &str) {
    let reference = common::kotlinc_box_result(main);
    assert_eq!(
        reference, "OK",
        "{stem}: the reference compiler disagrees: {reference}"
    );
    common::expect_box_ok_files_with_stdlib(&[("Main.kt", main)], stem);
}

const DECLARATIONS: &str = "interface P {\n\
\x20   val tag: String\n\
}\n\
\n\
class A : P {\n\
\x20   override val tag: String = \"a\"\n\
}\n\
\n\
class B : P {\n\
\x20   override val tag: String = \"b\"\n\
}\n\
\n";

/// The failing shape: both operands are calls whose element type is inferred, and only the declared
/// result relates them.
#[test]
fn a_declared_result_seeds_a_plus_over_two_inferred_operands() {
    let main = format!(
        "{DECLARATIONS}\
fun pick(): List<P> = listOf(A()) + listOf(B())\n\
fun box(): String {{\n\
\x20   val tags = pick().map {{ it.tag }}\n\
\x20   return if (tags == listOf(\"a\", \"b\")) \"OK\" else \"FAIL: \" + tags\n\
}}\n"
    );
    both_compilers_box(&main, "plus_declared_result");
}

/// The `fold` spelling the corpus actually uses: the accumulator is the receiver and the expected
/// result comes from the accumulator's own declared type.
#[test]
fn a_fold_accumulator_seeds_its_operator() {
    let main = format!(
        "{DECLARATIONS}\
fun merge(parts: List<A>): List<P> = parts.fold(listOf<P>()) {{ acc, part -> acc + listOf(part) }}\n\
fun box(): String {{\n\
\x20   val tags = merge(listOf(A(), A())).map {{ it.tag }}\n\
\x20   return if (tags == listOf(\"a\", \"a\")) \"OK\" else \"FAIL: \" + tags\n\
}}\n"
    );
    both_compilers_box(&main, "plus_fold_accumulator");
}

/// Control: an explicit type argument pins `T` before the operator runs, and always worked.
#[test]
fn an_explicit_type_argument_on_the_receiver_still_works() {
    let main = format!(
        "{DECLARATIONS}\
fun pick(): List<P> = listOf<P>(A()) + listOf(B())\n\
fun box(): String {{\n\
\x20   val tags = pick().map {{ it.tag }}\n\
\x20   return if (tags == listOf(\"a\", \"b\")) \"OK\" else \"FAIL: \" + tags\n\
}}\n"
    );
    both_compilers_box(&main, "plus_explicit_targ");
}

/// Control: a declared `val` receiver pins `T` the same way.
#[test]
fn a_declared_receiver_val_still_works() {
    let main = format!(
        "{DECLARATIONS}\
fun pick(): List<P> {{\n\
\x20   val first: List<P> = listOf(A())\n\
\x20   return first + listOf(B())\n\
}}\n\
fun box(): String {{\n\
\x20   val tags = pick().map {{ it.tag }}\n\
\x20   return if (tags == listOf(\"a\", \"b\")) \"OK\" else \"FAIL: \" + tags\n\
}}\n"
    );
    both_compilers_box(&main, "plus_declared_val");
}

/// Seeding from the expectation must not accept operands that genuinely do not fit: an `Int` element
/// has no common element type with the declared `List<P>` result.
#[test]
fn an_operand_that_does_not_fit_the_expectation_is_still_rejected() {
    let main = format!("{DECLARATIONS}fun pick(): List<P> = listOf(A()) + listOf(1)\n");
    let result = common::compiler_diagnostics(&[("Main.kt", &main)], &[]);
    assert_ne!(
        result.reference_code, 0,
        "kotlinc must reject an Int element against a List<P> result: {}",
        result.reference_stderr
    );
    assert_ne!(
        result.krusty_code, 0,
        "krusty accepted an operand that cannot fit the expectation: {}{}",
        result.krusty_stdout, result.krusty_stderr
    );
}
