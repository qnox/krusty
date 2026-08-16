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
