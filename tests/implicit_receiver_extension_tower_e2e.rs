//! A bare call inside a class selects an applicable extension on an implicit receiver ahead of
//! every receiver-less top-level declaration with the same name, whichever scope (current package,
//! explicit import or default import) declares either one. In kotlinc's tower each implicit
//! receiver occupies a lexical depth, and its extension levels — over every non-local scope — sit
//! at that depth; receiver-less top-level functions live in the file and import scopes, which are
//! always deeper. So `run { }` in a member is `this.run { }` (`T.run`), not `run(block)`.

use super::common;

/// An anonymous object inside a member's `run { }` reads a class property. kotlinc selects
/// `T.run`, so the property is read through the lambda's extension receiver and the object captures
/// that receiver. The checker used to select the receiver-less `run` and leave the object body's
/// receiver pointing at a lambda receiver that no longer existed (an internal lowering error).
#[test]
fn anonymous_object_in_member_run_reads_the_lambda_receiver() {
    common::expect_box_ok_with_stdlib(
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

/// The receiver extension wins even against a receiver-less function declared in the same package.
#[test]
fn implicit_receiver_extension_precedes_same_package_receiverless_function() {
    common::expect_box_ok_with_stdlib(
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
