//! Reading a member off a USE-SITE PROJECTED type. Everything read out of `List<out Range>` is a
//! `Range`, so `first()` returns a `Range` and its members resolve. krusty let the generic-argument
//! projection escape as the call expression's top-level type, so `c[0].startOffset` resolved (the
//! indexed-member path specializes output position) while `c.first().startOffset` did not.
//!
//! Inferring a type parameter FROM a projected receiver is a separate gap and is not fixed here:
//! `c.map { it.startOffset }` still fails, because `T` does not bind from `List<out Range>` and the
//! lambda body is then checked against an inapplicable candidate. See `docs/SPEC.md`.

use super::common;

fn run(source: &str) -> Option<String> {
    common::compile_and_run_box(
        source,
        "Main",
        std::slice::from_ref(&common::stdlib_jar()),
        Some(common::jdk_modules().as_path()),
    )
}

#[test]
fn a_member_reads_off_a_projected_extension_result() {
    let source = r#"
        class Range(val startOffset: Int, val endOffset: Int)

        fun firstStart(ranges: List<out Range>): Int = ranges.first().startOffset

        fun box(): String = "${firstStart(listOf(Range(7, 9)))}"
    "#;

    assert_eq!(run(source).as_deref(), Some("7"));
}

#[test]
fn a_projected_element_reads_the_same_as_an_indexed_one() {
    let source = r#"
        class Range(val startOffset: Int, val endOffset: Int)

        fun ends(ranges: List<out Range>): String =
            "${ranges[0].endOffset}${ranges.last().endOffset}"

        fun box(): String = ends(listOf(Range(1, 2), Range(3, 4)))
    "#;

    assert_eq!(run(source).as_deref(), Some("24"));
}

#[test]
fn an_invariant_receiver_still_reads_its_members() {
    let source = r#"
        class Range(val startOffset: Int, val endOffset: Int)

        fun starts(ranges: List<Range>): List<Int> = ranges.map { it.startOffset }

        fun box(): String = starts(listOf(Range(5, 6))).joinToString(",")
    "#;

    assert_eq!(run(source).as_deref(), Some("5"));
}
#[test]
fn a_contravariant_receiver_infers_an_anonymous_generic_extension_result() {
    let source = r#"
interface Invariant<A>
fun Invariant<in Number>.publicFunc() = privateFunc()
private fun <B> Invariant<B>.privateFunc() = object : Invariant<B> {}
fun box() = "OK"
"#;
    super::common::expect_box_same_as_kotlinc(
        source,
        "contravariant_receiver_anonymous_generic_result",
    );
}

const CAPTURED_RECEIVERS: &str = r#"
interface Token<A>
private fun <T> Token<T>.copy() = object : Token<T> {}
private fun <T> Token<T>.echo(value: T) = value
fun invariant(value: Token<Number>) = value.copy()
fun contravariant(value: Token<in Number>) = value.copy()
fun covariant(value: Token<out Number>) = value.copy()
fun star(value: Token<*>) = value.copy()
fun writable(value: Token<in Number>): Any? = value.echo(7)
fun box(): String {
    val numbers = object : Token<Number> {}
    val anything = object : Token<Any?> {}
    invariant(numbers)
    contravariant(anything)
    covariant(numbers)
    star(anything)
    return if (writable(anything) == 7) "OK" else "value"
}
"#;

#[test]
fn receiver_captures_preserve_invariant_covariant_contravariant_and_star_results() {
    common::expect_box_same_as_kotlinc(CAPTURED_RECEIVERS, "projected_receiver_capture_results");
}
