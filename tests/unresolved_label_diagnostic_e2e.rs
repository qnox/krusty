//! An unresolvable `return@label` reports kotlinc's diagnostic, at kotlinc's span.
//!
//! krusty rendered its own wording at the `return` keyword:
//!
//! ```text
//! krusty:  L.kt:4:11: error: return label 'Missing' does not denote an enclosing lambda
//! kotlinc: L.kt:4:17: error: unresolved label.
//! ```
//!
//! Both the message and the column differed, for every label that does not resolve — an unknown name
//! and a name that belongs to a lambda which is not enclosing alike. kotlinc points at the `@` token,
//! so the parser now keeps that span and both report sites use it.
//!
//! These are exact differential assertions: the complete diagnostic set of each compiler is compared
//! record by record, so an unrelated rejection cannot satisfy them.

use super::common;

/// A label naming nothing at all.
#[test]
fn an_unknown_return_label_matches_the_reference_diagnostic() {
    const MAIN: &str = "fun run(body: () -> Unit) {\n\
\x20   body()\n\
}\n\
\n\
fun probe() {\n\
\x20   run { return@Missing }\n\
}\n";
    let result = common::compiler_diagnostics(&[("Main.kt", MAIN)], &[]);
    common::expect_identical_rejection(&result, "an unknown return label");
}

/// A label that is genuinely DECLARED, on a sibling lambda that does not enclose the return.
///
/// `outer@` is written on the first LAMBDA, so the name exists in the file and resolves to a real
/// lambda label — it simply does not enclose the second lambda. (Writing `outer@` before the
/// statement instead labels the statement, and kotlinc then reports `target label does not denote a
/// function.`, which is a different divergence and not this one.) That is a different case from a name
/// nobody declared, and it is the one worth pinning: kotlinc reports the same `unresolved label.`
/// for both, so the two must not diverge.
#[test]
fn a_declared_but_non_enclosing_return_label_matches_the_reference_diagnostic() {
    const MAIN: &str = "fun probe(xs: List<Int>): Int {\n\
\x20   xs.forEach outer@ { return@outer }\n\
\x20   xs.forEach { return@outer }\n\
\x20   return 0\n\
}\n";
    let result = common::compiler_diagnostics(&[("Main.kt", MAIN)], &[]);
    common::expect_identical_rejection(&result, "a declared but non-enclosing return label");
}

/// The same shape in EXPRESSION position (`?: return@Missing`), which reports through a different
/// site in the checker and so needs its own span.
#[test]
fn an_unresolved_label_in_expression_position_matches_the_reference_diagnostic() {
    const MAIN: &str = "fun pick(value: Int?): Int {\n\
\x20   val chosen = value ?: return@Missing\n\
\x20   return chosen\n\
}\n";
    let result = common::compiler_diagnostics(&[("Main.kt", MAIN)], &[]);
    common::expect_identical_rejection(&result, "an unresolved label in expression position");
}

/// The control: a label that DOES denote an enclosing lambda still compiles and still returns from
/// the labelled lambda rather than the function, asserted against the reference compiler's own run.
#[test]
fn a_resolvable_return_label_still_works() {
    const MAIN: &str = "fun box(): String {\n\
\x20   val seen = StringBuilder()\n\
\x20   listOf(1, 2, 3).forEach {\n\
\x20       if (it == 2) return@forEach\n\
\x20       seen.append(it)\n\
\x20   }\n\
\x20   return if (seen.toString() == \"13\") \"OK\" else \"FAIL: \" + seen\n\
}\n";
    let reference = common::kotlinc_box_result(MAIN);
    assert_eq!(reference, "OK", "reference disagrees: {reference}");
    common::expect_box_ok_files_with_stdlib(&[("Main.kt", MAIN)], "resolvable_return_label");
}

/// A RETAINED body whose arena ids shift under compaction.
///
/// An `inline` function's body survives past Pass-1, while the bodies before it are discarded, so
/// the statement and expression ids inside it are renumbered. The label spans are arena-keyed side
/// tables, so they have to move with that renumbering; a table left behind reads a stale id and
/// underlines the wrong token, or reports no span at all. The discarded function ahead of it exists
/// precisely to make the ids shift.
#[test]
fn a_retained_inline_body_keeps_its_label_span_through_compaction() {
    const MAIN: &str = "fun discarded(): Int {\n\
\x20   val first = 1\n\
\x20   val second = first + 1\n\
\x20   val third = second + first\n\
\x20   return third\n\
}\n\
\n\
inline fun retained(body: () -> Unit) {\n\
\x20   body()\n\
\x20   run { return@Missing }\n\
}\n";
    let result = common::compiler_diagnostics(&[("Main.kt", MAIN)], &[]);
    common::expect_identical_rejection(&result, "a retained inline body after compaction");
}

/// The same, in EXPRESSION position inside the retained body — the other span table.
#[test]
fn a_retained_inline_body_keeps_its_expression_label_span_through_compaction() {
    const MAIN: &str = "fun discarded(): Int {\n\
\x20   val first = 1\n\
\x20   val second = first + 1\n\
\x20   val third = second + first\n\
\x20   return third\n\
}\n\
\n\
inline fun retained(value: Int?): Int {\n\
\x20   val chosen = value ?: return@Missing\n\
\x20   return chosen\n\
}\n";
    let result = common::compiler_diagnostics(&[("Main.kt", MAIN)], &[]);
    common::expect_identical_rejection(&result, "a retained inline expression after compaction");
}
