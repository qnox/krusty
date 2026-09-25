//! `inline fun <reified T> … = x is T` — a declaration whose type parameter is REIFIED.
//!
//! Kotlin permits a reified type parameter only on an `inline` function, and such a function is
//! spliced at every call site precisely so that `is T` and `T::class` have a type to name. Its own
//! body is therefore never called, and compiling it would have to answer what `T` is where nothing
//! has said. The native target emits no body for one, which is the same treatment a lambda that
//! returns non-locally already gets.
//!
//! Every expectation is kotlinc's, taken by running the same program under it.

use super::common::{expect_box_ok_with_stdlib, expect_native_box, kotlinc_box_result};

/// Require kotlinc's answer, krusty's JVM answer and the NATIVE answer to agree.
fn every_backend_agrees_with_kotlinc(stem: &str, source: &str) {
    assert_eq!(
        kotlinc_box_result(source),
        "OK",
        "{stem}: unexpected kotlinc result"
    );
    expect_box_ok_with_stdlib(source, stem);
    expect_native_box(source, stem, "OK");
}

/// The same at top level, where no table is involved and only the symbol is.
#[test]
fn a_top_level_reified_declaration_that_lowers_keeps_its_body() {
    let source = "inline fun <reified T, U> keep(value: U): U = value\n\
         fun box(): String = if (keep<String, Int>(42) == 42) \"OK\" else \"fail\"\n";
    every_backend_agrees_with_kotlinc("ReifiedTopLevelKeepsItsBody", source);
}
