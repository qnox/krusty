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

/// `is T` inside a reified inline function, answered at each call site with that site's type.
#[test]
fn a_reified_type_check_answers_the_type_the_call_site_named() {
    let source = "class A\n\
         class B\n\
         inline fun <reified T> isT(x: Any): Boolean = x is T\n\
         fun box(): String {\n\
         \x20   if (!isT<A>(A())) return \"fail A is A\"\n\
         \x20   if (isT<B>(A())) return \"fail A is B\"\n\
         \x20   return \"OK\"\n\
         }\n";
    every_backend_agrees_with_kotlinc("ReifiedTypeCheck", source);
}

/// The NULLABLE form, which admits null whatever `T` is.
#[test]
fn a_reified_nullable_type_check_admits_null() {
    let source = "class A\n\
         class B\n\
         inline fun <reified T> Any?.isTOrNull(): Boolean = this is T?\n\
         fun box(): String {\n\
         \x20   if (!null.isTOrNull<A>()) return \"fail null\"\n\
         \x20   if (!A().isTOrNull<A>()) return \"fail A\"\n\
         \x20   if (A().isTOrNull<B>()) return \"fail A is B\"\n\
         \x20   return \"OK\"\n\
         }\n";
    every_backend_agrees_with_kotlinc("ReifiedNullableTypeCheck", source);
}

/// `T::class` reads the site's type too, and its SIMPLE NAME is the last segment.
#[test]
fn a_reified_class_literal_names_the_type_the_call_site_named() {
    let source = "class Named\n\
         inline fun <reified T> nameOf(): String? = T::class.simpleName\n\
         fun box(): String {\n\
         \x20   return if (nameOf<Named>() == \"Named\") \"OK\" else \"fail \" + nameOf<Named>()\n\
         }\n";
    every_backend_agrees_with_kotlinc("ReifiedClassLiteralName", source);
}

/// A NESTED class's simple name is the part after the nesting, not the whole spelling. A companion
/// is the case the corpus tests, and a plain nested class is the same question.
#[test]
fn a_nested_classs_simple_name_drops_what_encloses_it() {
    let source = "class Outer {\n\
         \x20   class Inner\n\
         \x20   companion object\n\
         }\n\
         fun box(): String {\n\
         \x20   if (Outer.Inner::class.simpleName != \"Inner\") return \"fail inner\"\n\
         \x20   if (Outer.Companion::class.simpleName != \"Companion\") return \"fail companion\"\n\
         \x20   if (Outer::class.simpleName != \"Outer\") return \"fail outer\"\n\
         \x20   return \"OK\"\n\
         }\n";
    every_backend_agrees_with_kotlinc("NestedSimpleName", source);
}

/// A NAMED companion answers its own name rather than `Companion`.
#[test]
fn a_named_companions_simple_name_is_the_name_it_was_given() {
    let source = "class Holder {\n\
         \x20   companion object MyCompanion {\n\
         \x20       fun name(): String? = this::class.simpleName\n\
         \x20   }\n\
         }\n\
         fun box(): String {\n\
         \x20   return if (Holder.MyCompanion.name() == \"MyCompanion\") \"OK\"\n\
         \x20       else \"fail \" + Holder.MyCompanion.name()\n\
         }\n";
    every_backend_agrees_with_kotlinc("NamedCompanionSimpleName", source);
}

/// A reified declaration whose body DOES lower keeps it, and a call reaches it.
///
/// `keep` names `T` nowhere, so nothing about its body needs the type a call site substitutes —
/// and the trap is only for a body this generator cannot lower. Emitting one for every reified
/// declaration instead left a MEMBER's vtable slot with no symbol to name, which is a table that
/// cannot be built rather than a call that declines.
#[test]
fn a_reified_declaration_that_lowers_keeps_its_body() {
    let source = "class Host {\n\
         \x20   inline fun <reified T, U> keep(value: U): U = value\n\
         }\n\
         fun box(): String =\n\
         \x20   if (Host().keep<String, Int>(42) == 42) \"OK\" else \"fail\"\n";
    every_backend_agrees_with_kotlinc("ReifiedMemberKeepsItsBody", source);
}

/// The same at top level, where no table is involved and only the symbol is.
#[test]
fn a_top_level_reified_declaration_that_lowers_keeps_its_body() {
    let source = "inline fun <reified T, U> keep(value: U): U = value\n\
         fun box(): String = if (keep<String, Int>(42) == 42) \"OK\" else \"fail\"\n";
    every_backend_agrees_with_kotlinc("ReifiedTopLevelKeepsItsBody", source);
}
