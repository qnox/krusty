//! A bare call inside a class selects an applicable extension on an implicit receiver ahead of
//! every receiver-less top-level declaration with the same name, whichever scope (current package,
//! explicit import or default import) declares either one. In kotlinc's tower each implicit
//! receiver occupies a lexical depth, and its extension levels — over every non-local scope — sit
//! at that depth; receiver-less top-level functions live in the file and import scopes, which are
//! always deeper. So `run { }` in a member is `this.run { }` (`T.run`), not `run(block)`.
//!
//! Every case runs under kotlinc and Krusty and requires the same `box()` result.

use super::common;

/// The generic candidate rule, with no standard-library name involved: a repository-owned
/// extension imported from a fixture package competes with a receiver-less function of the same
/// name declared in the calling package. The extension on the implicit `this` wins.
#[test]
fn imported_fixture_extension_on_implicit_receiver_precedes_same_package_receiverless_function() {
    const EXTENSION: &str = "package fixtures.towerext\n\
         \n\
         fun <T, R> T.choose(block: T.() -> R): R = block()\n";
    const MAIN: &str = "package fixtures.towermain\n\
         \n\
         import fixtures.towerext.choose\n\
         \n\
         var picked = \"\"\n\
         \n\
         fun <R> choose(block: () -> R): R {\n\
         \x20   picked = \"receiver-less\"\n\
         \x20   return block()\n\
         }\n\
         \n\
         class Holder(val value: String) {\n\
         \x20   fun get(): String = choose { value }\n\
         }\n\
         \n\
         fun box(): String {\n\
         \x20   val result = Holder(\"OK\").get()\n\
         \x20   return if (picked.isEmpty()) result else picked\n\
         }\n";
    let sources = [("Ext.kt", EXTENSION), ("Main.kt", MAIN)];
    assert_eq!(
        common::kotlinc_box_files_result(&sources, "fixtures.towermain.MainKt"),
        "OK",
        "kotlinc selects the imported extension on the implicit receiver",
    );
    common::expect_box_ok_files_with_stdlib(
        &sources,
        "an imported extension on an implicit receiver beside a same-package receiver-less function",
    );
}

/// The same rule for the default-imported `T.run` against a same-package receiver-less `run`.
#[test]
fn implicit_receiver_extension_precedes_same_package_receiverless_function() {
    common::expect_box_same_as_kotlinc(
        "var picked = \"\"\n\
         fun <R> run(block: () -> R): R { picked = \"receiver-less\"; return block() }\n\
         class Holder { fun get(): String = run { \"OK\" } }\n\
         fun box(): String {\n\
             val result = Holder().get()\n\
             return if (picked.isEmpty()) result else picked\n\
         }\n",
        "ImplicitReceiverRunPrecedence",
    );
}

/// An anonymous object inside a member's `run { }` reads a class property. kotlinc selects
/// `T.run`, so the property is read through the lambda's extension receiver and the object captures
/// that receiver. The checker used to select the receiver-less `run` and leave the object body's
/// receiver pointing at a lambda receiver that no longer existed (an internal lowering error).
#[test]
fn anonymous_object_in_member_run_reads_the_lambda_receiver() {
    common::expect_box_same_as_kotlinc(
        "abstract class Source { abstract fun read(): String }\n\
         class Holder(private val value: String) {\n\
             fun get(): String = run {\n\
                 object : Source() {\n\
                     override fun read() = value\n\
                 }.read()\n\
             }\n\
         }\n\
         fun box(): String = Holder(\"OK\").get()\n",
        "ImplicitReceiverRunAnonymousObject",
    );
}
