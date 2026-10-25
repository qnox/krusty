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

#[test]
fn receiver_captures_reject_incompatible_explicit_arguments_and_closed_writes() {
    let source = r#"
interface Token<A>
fun <T> Token<T>.echo(value: T): T = value
fun wrongExplicit(value: Token<in Number>) = value.echo<String>("wrong")
fun closedOut(value: Token<out Number>) = value.echo(7)
fun closedStar(value: Token<*>) = value.echo(7)
"#;
    common::assert_errors_match_kotlinc(&[("Main.kt", source)], &[]);
}

#[test]
fn closed_receiver_input_named_argument_diagnostics_match_kotlinc() {
    let source = r#"
interface Bucket<A>
fun <T> Bucket<T>.put(prefix: String, value: T) {}
fun closed(value: Bucket<out Number>) = value.put(value = 7, prefix = "ok")
fun star(value: Bucket<*>) = value.put(prefix = "ok", value = 7)
"#;
    common::assert_errors_match_kotlinc(&[("Main.kt", source)], &[]);
}

#[test]
fn named_whole_array_varargs_retain_the_mapped_array_slot_in_recovery() {
    let source = r#"
interface Bucket<A>
fun <T> Bucket<T>.put(vararg values: T, prefix: String) {}
fun wrong(value: Bucket<Number>, elements: Array<Number>) =
    value.put(values = elements, prefix = 7)
"#;
    common::assert_errors_match_kotlinc(&[("Main.kt", source)], &[]);
}

#[test]
fn closed_receiver_input_nullable_argument_diagnostics_match_kotlinc() {
    let source = r#"
interface Bucket<A>
fun <T> Bucket<T>.put(value: T?) {}
fun allowed(value: Bucket<out Number>, star: Bucket<*>) {
    value.put(null)
    star.put(null)
}
fun closed(value: Bucket<out Number>) = value.put(7)
fun star(value: Bucket<*>) = value.put(7)
"#;
    common::assert_errors_match_kotlinc(&[("Main.kt", source)], &[]);
}

#[test]
fn closed_receiver_input_unrelated_argument_diagnostics_match_kotlinc() {
    let source = r#"
interface Bucket<A>
fun <T> Bucket<T>.fixed(value: String) {}
fun <T> Bucket<T>.bottom(value: Nothing) {}
fun unrelated(value: Bucket<out Number>) = value.fixed(7)
fun declaredBottom(value: Bucket<*>) = value.bottom(7)
"#;
    common::assert_errors_match_kotlinc(&[("Main.kt", source)], &[]);
}

#[test]
fn closed_receiver_input_context_argument_diagnostics_match_kotlinc() {
    let source = r#"
// LANGUAGE: +ContextParameters
interface Bucket<A>
context(prefix: String)
fun <T> Bucket<T>.put(value: T) {}
context(prefix: String)
fun closed(value: Bucket<out Number>) = value.put(7)
context(prefix: String)
fun star(value: Bucket<*>) = value.put(7)
"#;
    common::assert_errors_match_kotlinc(&[("Main.kt", source)], &[]);
}

#[test]
fn direct_receiver_inputs_follow_each_declared_projection() {
    let source = r#"
interface Bucket<A, B>
fun <A, B> Bucket<A, B>.first(value: A) {}
fun <A, B> Bucket<A, B>.second(value: B) {}
fun outThenStar(value: Bucket<out Number, *>) {
    value.first(7)
    value.second(7)
}
fun starThenOut(value: Bucket<*, out Number>) {
    value.first(7)
    value.second(7)
}
"#;
    common::assert_errors_match_kotlinc(&[("Main.kt", source)], &[]);
}
