//! A star projection satisfies an extension receiver `C<T?>` whose `T` is bounded by `Any`.
//!
//! `List<*>` reads as `List<out Any?>`, so `Iterable<T?>` matches with `T = Any` — the `?` in the
//! formal absorbs the star's nullability and leaves the non-null bound satisfied. krusty bound `T`
//! to the star's readable upper `Any?` directly, which its `T : Any` bound rejects, and reported
//! "none of the following candidates is applicable". `filterNotNull` is the shape that matters:
//! `fun <T : Any> Iterable<T?>.filterNotNull(): List<T>` was unusable on any starred collection.

use super::common;

/// Compile one fixture with both compilers and require the exact same successful contract.
fn assert_both_accept(sources: &[(&str, &str)], what: &str) {
    let result = common::compiler_diagnostics(sources, &[common::stdlib_jar()]);
    assert_eq!(
        (result.reference_code, result.reference_stderr.as_str()),
        (0, ""),
        "kotlinc rejected the {what} fixture"
    );
    assert_eq!(
        (result.krusty_code, result.krusty_stderr.as_str()),
        (0, ""),
        "krusty must accept the exact source set kotlinc accepts for {what}"
    );
}

/// The declaration shape in source.
#[test]
fn a_star_projection_satisfies_a_nullable_formal_receiver() {
    assert_both_accept(
        &[(
            "Drop.kt",
            "fun <T : Any> Iterable<T?>.dropNulls(): List<T> = filterNotNull()\n\
             fun use(xs: List<*>): List<Any> = xs.dropNulls()\n",
        )],
        "`Iterable<T?>` with a starred receiver",
    );
}

/// The stdlib shape the corpus hit.
#[test]
fn filter_not_null_accepts_a_star_projected_receiver() {
    assert_both_accept(
        &[(
            "Filter.kt",
            "fun use(xs: List<*>): List<Any> = xs.filterNotNull()\n",
        )],
        "`filterNotNull` with a starred receiver",
    );
}

/// The receiver may also be declared on the subtype itself, not only on `Iterable`. The body is
/// deliberately trivial: an implicit `List<T?>` receiver reaching `Iterable<T?>.filterNotNull` is a
/// separate, still-open gap, and this case is about the CALL site.
#[test]
fn a_star_projection_satisfies_a_nullable_formal_on_the_exact_classifier() {
    assert_both_accept(
        &[(
            "Exact.kt",
            "fun <T : Any> List<T?>.dropNulls(): List<T> = emptyList()\n\
             fun use(xs: List<*>): List<Any> = xs.dropNulls()\n",
        )],
        "`List<T?>` with a starred receiver",
    );
}

/// Control: an unbounded formal keeps working, and the element type stays nullable.
#[test]
fn an_unbounded_formal_keeps_the_stars_nullability() {
    assert_both_accept(
        &[(
            "Open.kt",
            "fun <T> Iterable<T>.all(): List<T> = toList()\n\
             fun use(xs: List<*>): List<Any?> = xs.all()\n",
        )],
        "an unbounded formal with a starred receiver",
    );
}

/// Use-site variance is not interchangeable. A covariant actual contributes its readable upper
/// bound, while a contravariant actual can only be read as `Any?`; opening `in String` to `String`
/// would invent a more precise result than Kotlin permits.
#[test]
fn nullable_formals_respect_use_site_projection_direction() {
    assert_both_accept(
        &[(
            "Variance.kt",
            "class Box<T>\n\
             fun <T : Any> Iterable<T?>.boxed(): Box<T> = Box()\n\
             fun fromIn(xs: MutableList<in String>): Box<Any> = xs.boxed()\n\
             fun fromOut(xs: MutableList<out Number?>): Box<Number> = xs.boxed()\n",
        )],
        "nullable formal with contravariant and covariant actuals",
    );
}

/// Control: a NON-nullable formal `Iterable<T>` with `T : Any` genuinely does not accept `List<*>`.
/// Kotlinc reports both the enclosing return mismatch and the rejected receiver candidate, and so
/// must krusty; the exact comparison also prevents the fix from making every star fit a non-null
/// bound.
#[test]
fn a_star_projection_still_fails_a_non_nullable_formal_receiver() {
    let result = common::compiler_diagnostics(
        &[(
            "Strict.kt",
            "fun <T : Any> Iterable<T>.only(): List<T> = toList()\n\
             fun use(xs: List<*>): List<Any> = xs.only()\n",
        )],
        &[common::stdlib_jar()],
    );
    assert_eq!(result.reference_code, 1, "{}", result.reference_stderr);
    assert_eq!(
        common::compiler_errors(&result.krusty_stderr),
        common::compiler_errors(&result.reference_stderr),
        "the selected kotlinc's complete ordered error headers"
    );
    assert_eq!(result.krusty_code, 1, "{}", result.krusty_stderr);
}

/// The runtime contract: the starred receiver really reaches `filterNotNull`, so the selected
/// callee is the one kotlinc selects rather than merely a type that checks.
#[test]
fn a_star_projected_receiver_drops_nulls_at_runtime() {
    common::expect_box_ok_files_with_stdlib(
        &[(
            "Star.kt",
            "fun drop(xs: List<*>): List<Any> = xs.filterNotNull()\n\
             fun box(): String {\n\
             \x20   val got = drop(listOf(\"a\", null, \"b\"))\n\
             \x20   val joined = got.joinToString(\",\")\n\
             \x20   return if (joined == \"a,b\") \"OK\" else joined\n\
             }\n",
        )],
        "StarProjectionNullableReceiver",
    );
}
