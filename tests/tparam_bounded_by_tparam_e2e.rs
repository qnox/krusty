//! `T : R` is a LOWER bound on `R`: the binding for `T` constrains `R` even when `R` also occurs in
//! a value parameter.
//!
//! `Result.getOrElse` is declared `<R, T : R> Result<T>.getOrElse(onFailure: (Throwable) -> R): R`.
//! With no expected type, `runCatching { xs.toSet() }.getOrElse { emptySet() }` gives the solver
//! only `T = Set<String>` from the receiver — the lambda's `emptySet()` is itself generic. krusty
//! treated `R` as "directly constrained" (it appears in the lambda's return position) and therefore
//! declined to propagate the `T : R` edge at all, leaving `R` unsolved and reporting
//! "none of the following candidates is applicable" against a receiver erased to `Result<Any>`.
//!
//! `ifEmpty` is the other half: `<C, R> C.ifEmpty(defaultValue: () -> R): R where C : CharSequence,
//! C : R` needs `R` to be the JOIN of the receiver and the lambda result, and taking either side
//! alone leaves `C : R` unsatisfiable.

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

/// The declaration shape on its own, in source: no stdlib inference to hide behind.
#[test]
fn a_formal_bounded_by_another_formal_seeds_it_from_the_receiver() {
    assert_both_accept(
        &[(
            "Box.kt",
            "class Box<out T>(val v: T)\n\
             fun <R, T : R> Box<T>.orElse(fallback: () -> R): R = v\n\
             fun use(xs: List<String>) {\n\
             \x20   val a = Box(xs.toSet()).orElse { emptySet() }\n\
             \x20   println(a)\n\
             }\n",
        )],
        "source-declared `T : R`",
    );
}

/// The stdlib shape the corpus actually hit.
#[test]
fn get_or_else_infers_from_the_receiver_without_an_expected_type() {
    assert_both_accept(
        &[(
            "Run.kt",
            "fun use(xs: List<String>) {\n\
             \x20   val a = runCatching { xs.toSet() }.getOrElse { emptySet() }\n\
             \x20   println(a)\n\
             }\n",
        )],
        "`getOrElse` with no expected type",
    );
}

/// The inferred type is usable at the receiver's ELEMENT type, which pins `R` to `Set<String>`
/// rather than to a raw `Set` or to `Any`.
#[test]
fn the_inferred_result_keeps_the_receivers_element_type() {
    assert_both_accept(
        &[(
            "Elem.kt",
            "fun use(xs: List<String>): Int {\n\
             \x20   val a = runCatching { xs.toSet() }.getOrElse { emptySet() }\n\
             \x20   return a.sumOf { it.length }\n\
             }\n",
        )],
        "`getOrElse` element type",
    );
}

/// Control: concrete lambda evidence still widens the bounded formal rather than freezing it at the
/// receiver's binding.
#[test]
fn a_wider_lambda_result_still_widens_the_bounded_formal() {
    assert_both_accept(
        &[(
            "Wide.kt",
            "fun use(xs: List<String>) {\n\
             \x20   val a: Any = runCatching { xs.toSet() }.getOrElse { 0 }\n\
             \x20   println(a)\n\
             }\n",
        )],
        "a widening lambda result",
    );
}

/// The join half of the relation: receiver `String`, lambda result unrelated, so `R` is `Any`.
#[test]
fn a_formal_bounded_by_another_formal_joins_both_sides() {
    assert_both_accept(
        &[(
            "Join.kt",
            "sealed interface Kind\n\
             data object One : Kind\n\
             fun use(desc: String, k: Kind) {\n\
             \x20   val v = desc.ifEmpty { k }\n\
             \x20   println(v)\n\
             }\n",
        )],
        "`ifEmpty` join",
    );
}

/// The same call inside a string template — the shape the corpus carries.
#[test]
fn the_join_holds_inside_a_string_template() {
    assert_both_accept(
        &[(
            "Template.kt",
            "sealed interface Kind\n\
             data object One : Kind\n\
             fun use(desc: String, k: Kind): String = \"x: ${desc.ifEmpty { k }}\"\n",
        )],
        "`ifEmpty` inside a template",
    );
}

/// The runtime contract: the inferred `R` really carries the receiver's value, so the bound edge
/// selected the callee kotlinc selects rather than merely type-checking.
#[test]
fn the_bounded_formal_carries_the_receivers_value_at_runtime() {
    common::expect_box_ok_files_with_stdlib(
        &[(
            "Box.kt",
            "fun pick(xs: List<String>): Set<String> =\n\
             \x20   runCatching { xs.toSet() }.getOrElse { emptySet() }\n\
             fun box(): String {\n\
             \x20   val got = pick(listOf(\"a\", \"b\"))\n\
             \x20   val joined = got.sorted().joinToString(\",\")\n\
             \x20   return if (joined == \"a,b\") \"OK\" else joined\n\
             }\n",
        )],
        "TparamBoundedByTparam",
    );
}
