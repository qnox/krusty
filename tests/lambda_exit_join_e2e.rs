//! A lambda's labelled returns and its tail join through the SYMBOL SOURCE.
//!
//! A lambda can leave through a labelled return and through its tail, and those exits can carry
//! different but related types. `flatMap` expects `Iterable<T>` while a tail that built a list
//! produces `List<T>`; `joinToString` expects `CharSequence` while a tail produces `String`.
//!
//! The exits were merged by TYPE IDENTITY alone: anything not equal (modulo nullability) collapsed to
//! `Any`, which then failed against the very expectation that produced one of the exits —
//!
//! ```text
//! error: type mismatch: inferred type is Any but Iterable<Item> was expected
//! ```
//!
//! `map` was unaffected because both of its exits are the element type itself, so they were equal.
//! The surrounding inference already joins through the symbol source, where a subtype yields its
//! supertype; this exit join now does too.

use super::common;

fn both_compilers_box(source: &str, stem: &str) {
    let reference = common::kotlinc_box_result(source);
    let krusty = common::expect_box_run_with_stdlib(source, stem);
    assert_eq!(reference, "OK", "{stem}: reference runtime");
    assert_eq!(krusty, reference, "{stem}: runtime differential");
}

/// The failing shape: `flatMap`, whose expected `Iterable<T>` differs from a `List<T>` tail.
#[test]
fn a_labeled_return_and_a_tail_join_to_their_supertype() {
    const MAIN: &str = "fun pieces(of: String): List<String> = listOf(of, of)\n\
fun concrete(keys: List<String>, skip: Boolean): List<String> =\n\
\x20   keys.flatMap { key ->\n\
\x20       if (skip) return@flatMap listOf<String>()\n\
\x20       pieces(key)\n\
\x20   }\n\
fun inferred(keys: List<String>, skip: Boolean): List<String> =\n\
\x20   keys.flatMap { key ->\n\
\x20       if (skip) return@flatMap emptyList()\n\
\x20       pieces(key)\n\
\x20   }\n\
fun box(): String {\n\
\x20   if (concrete(listOf(\"a\"), false) != listOf(\"a\", \"a\")) return \"FAIL: concrete kept\"\n\
\x20   if (concrete(listOf(\"a\"), true) != emptyList<String>()) return \"FAIL: concrete skipped\"\n\
\x20   if (inferred(listOf(\"b\"), false) != listOf(\"b\", \"b\")) return \"FAIL: inferred kept\"\n\
\x20   if (inferred(listOf(\"b\"), true) != emptyList<String>()) return \"FAIL: inferred skipped\"\n\
\x20   return \"OK\"\n\
}\n";
    both_compilers_box(MAIN, "lambda_exit_join_flat_map");
}

/// The same join with a non-collection pair: `joinToString` expects `CharSequence`, the tail gives
/// `String`. This is the corpus shape.
#[test]
fn a_char_sequence_expectation_joins_with_a_string_tail() {
    const MAIN: &str = "fun render(parts: List<String>): String =\n\
\x20   parts.joinToString(\"+\") { part ->\n\
\x20       if (part.isEmpty()) return@joinToString \"empty\"\n\
\x20       part.trim()\n\
\x20   }\n\
fun box(): String {\n\
\x20   if (render(listOf(\" a \", \"b\")) != \"a+b\") return \"FAIL: kept\"\n\
\x20   if (render(listOf(\"\")) != \"empty\") return \"FAIL: skipped\"\n\
\x20   return \"OK\"\n\
}\n";
    both_compilers_box(MAIN, "lambda_exit_join_char_sequence");
}

/// The shapes that already worked stay working: `map`, where both exits are the element type, and a
/// lambda whose only exit is its tail.
///
/// The element type here is a REFERENCE on purpose. A primitive element exercises a separate defect
/// in the bytecode splicer's result boxing, fixed on its own branch; using one here would couple
/// this test to that change instead of to the exit join under test.
#[test]
fn the_equal_exit_shapes_still_infer() {
    const MAIN: &str = "fun mapped(keys: List<String>, skip: Boolean): List<String> =\n\
\x20   keys.map { key -> if (skip) return@map \"none\"; key.trim() }\n\
fun tailOnly(keys: List<String>): List<String> = keys.map { it.trim() }\n\
fun box(): String {\n\
\x20   if (mapped(listOf(\" a \"), false) != listOf(\"a\")) return \"FAIL: map kept\"\n\
\x20   if (mapped(listOf(\" a \"), true) != listOf(\"none\")) return \"FAIL: map skipped\"\n\
\x20   if (tailOnly(listOf(\" b \")) != listOf(\"b\")) return \"FAIL: tail\"\n\
\x20   return \"OK\"\n\
}\n";
    both_compilers_box(MAIN, "equal_exit_shapes");
}

/// Every labelled return participates in the semantic join, not only the already-merged return
/// accumulator and the tail. A type-only first merge would collapse `First` and `Second` to `Any`
/// before the tail is considered.
#[test]
fn several_labeled_returns_join_before_the_tail() {
    const MAIN: &str = "interface Piece { fun text(): String }\n\
class First(private val value: String) : Piece { override fun text(): String = value }\n\
class Second(private val value: String) : Piece { override fun text(): String = value }\n\
fun infer(kinds: List<Int>) = kinds.map { kind ->\n\
\x20   if (kind == 0) return@map First(\"zero\")\n\
\x20   if (kind == 1) return@map Second(\"one\")\n\
\x20   First(\"tail\")\n\
}\n\
fun accept(parts: List<Piece>): String = parts.joinToString(\"|\") { it.text() }\n\
fun box(): String {\n\
\x20   val rendered = accept(infer(listOf(0, 1, 2)))\n\
\x20   return if (rendered == \"zero|one|tail\") \"OK\" else \"FAIL: \" + rendered\n\
}\n";
    both_compilers_box(MAIN, "several_lambda_exit_join");
}

/// Two exits with NO useful common supertype still join to `Any`, and that still fails against a
/// concrete expectation — joining through the source must not accept unrelated exits. Both
/// compilers' output is asserted.
#[test]
fn unrelated_exits_are_still_rejected() {
    const MAIN: &str = "fun pieces(of: String): List<String> = listOf(of)\n\
fun walk(keys: List<String>, skip: Boolean): List<String> =\n\
\x20   keys.flatMap { key ->\n\
\x20       if (skip) return@flatMap listOf(1)\n\
\x20       pieces(key)\n\
\x20   }\n";
    let result = common::compiler_diagnostics(&[("Main.kt", MAIN)], &[]);
    assert_eq!((result.krusty_code, result.reference_code), (1, 1));
    assert_eq!(common::compiler_errors(&result.krusty_stdout), []);
    assert_eq!(
        common::compiler_errors(&result.krusty_stderr),
        [common::CompilerError {
            file: "Main.kt".to_string(),
            line: 4,
            column: 34,
            message: "return type mismatch: expected 'Iterable<String>', actual 'List<Int>'."
                .to_string(),
        }]
    );
    assert_eq!(
        common::compiler_errors(&result.reference_stderr),
        [common::CompilerError {
            file: "Main.kt".to_string(),
            line: 3,
            column: 5,
            message: "return type mismatch: expected 'List<String>', actual 'List<Comparable<*> & Serializable>'.".to_string(),
        }]
    );
}
