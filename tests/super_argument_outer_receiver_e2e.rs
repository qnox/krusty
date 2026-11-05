//! A superclass constructor's arguments cannot use the instance under construction.
//!
//! When the enclosing instance a super-constructor argument names is a supertype of the class
//! being constructed, the argument still reads that enclosing instance: `token` in
//! `object : Holder(token)` inside `Holder` is the outer holder's, and the outer `Outer` of
//! `inner class Second : First(s)` is the constructor's outer parameter even though `Second` is
//! itself an `Outer`. krusty read both through the uninitialized `this` and failed verification.
use super::common;
use super::common::{compare_with_kotlinc_plugin, method_instructions};

const SOURCE: &str = "class Token\n\
open class Holder(val token: Token) {\n\
    fun copy(): Holder = object : Holder(token) {}\n\
}\n\
open class Outer(val s: String) {\n\
    open inner class First(s: String) : Outer(s)\n\
    inner class Second(s: String, extra: Long) : First(s)\n\
}\n";

#[test]
fn super_arguments_read_the_enclosing_instance() {
    common::expect_box_ok_with_stdlib(
        &format!(
            "{SOURCE}\
             fun box(): String {{\n\
             \x20   val token = Token()\n\
             \x20   if (Holder(token).copy().token !== token) return \"fail copy\"\n\
             \x20   return Outer(\"fail\").Second(\"OK\", 7L).s\n\
             }}\n"
        ),
        "SuperArgumentOuter",
    );
}

#[test]
fn inner_super_delegation_passes_the_outer_parameter_like_kotlinc() {
    let built = compare_with_kotlinc_plugin(
        "SuperArgumentOuter",
        SOURCE,
        "Outer$Second",
        &[common::stdlib_jar()],
        "25",
        &[],
    )
    .expect("reference kotlinc is provisioned");
    let member = "Outer$Second(Outer, java.lang.String, long)";
    let reference = method_instructions(&built.reference, member);
    assert!(!reference.is_empty(), "kotlinc: {member} not found");
    assert_eq!(method_instructions(&built.krusty, member), reference);
}

#[test]
fn an_anonymous_superclass_argument_cannot_read_its_inherited_property() {
    let source = "\nopen class Carrier(val value: String)\n\
        fun copy() = object : Carrier(value) {}\n";
    let diagnostics = common::compiler_diagnostics(
        &[("Main.kt", source)],
        &[common::stdlib_jar(), common::jdk_modules()],
    );
    assert_eq!(
        common::compiler_errors(&diagnostics.reference_stderr),
        vec![common::CompilerError {
            file: "Main.kt".to_owned(),
            line: 3,
            column: 31,
            message: "cannot access '<this>' before the instance has been initialized.".to_owned(),
        }]
    );
    common::expect_identical_rejection(
        &diagnostics,
        "anonymous superclass dispatch is unavailable",
    );
}

#[test]
fn an_unavailable_super_argument_receiver_is_independent_of_the_property_spelling() {
    let source = "\nopen class Carrier(val payload: String)\n\
        fun copy() = object : Carrier(payload) {}\n";
    let diagnostics = common::compiler_diagnostics(
        &[("Main.kt", source)],
        &[common::stdlib_jar(), common::jdk_modules()],
    );
    common::expect_identical_rejection(&diagnostics, "renamed unavailable inherited property");
}

#[test]
fn an_unavailable_super_argument_receiver_also_owns_an_inherited_accessor() {
    let source = "open class Carrier(val seed: String) { val value: String get() = seed }\n\
        fun copy() = object : Carrier(value) {}\n";
    let diagnostics = common::compiler_diagnostics(
        &[("Main.kt", source)],
        &[common::stdlib_jar(), common::jdk_modules()],
    );
    common::expect_identical_rejection(&diagnostics, "unavailable inherited accessor receiver");
}

#[test]
fn a_lexical_parameter_precedes_an_unavailable_inherited_property() {
    let source = "open class Carrier(val value: String)\n\
        fun copy(value: String): Carrier = object : Carrier(value) {}\n\
        fun box(): String = copy(\"OK\").value\n";
    common::expect_box_ok_with_stdlib(source, "SuperArgumentLexicalValue");
}

#[test]
fn an_available_outer_receiver_precedes_the_unavailable_instance() {
    let source = "open class Carrier(val value: String) {\n\
        \x20   fun copy(): Carrier = object : Carrier(value) {}\n\
        }\n\
        fun box(): String = Carrier(\"OK\").copy().value\n";
    common::expect_box_ok_with_stdlib(source, "SuperArgumentAvailableOuterValue");
}

#[test]
fn an_inferred_inherited_getter_still_requires_an_initialized_receiver() {
    let source = "open class Carrier(val seed: String) { val value get() = seed }\n\
        fun copy() = object : Carrier(value) {}\n";
    let diagnostics = common::compiler_diagnostics(
        &[("Main.kt", source)],
        &[common::stdlib_jar(), common::jdk_modules()],
    );
    common::expect_identical_rejection(&diagnostics, "inferred unavailable inherited getter");
}

#[test]
fn a_named_superclass_argument_keeps_the_receiver_initialization_diagnostic() {
    let source = "open class Carrier(val value: String)\n\
        class Derived : Carrier(value)\n";
    let diagnostics = common::compiler_diagnostics(
        &[("Main.kt", source)],
        &[common::stdlib_jar(), common::jdk_modules()],
    );
    common::expect_identical_rejection(&diagnostics, "named superclass unavailable receiver");
}

#[test]
fn a_top_level_value_precedes_an_unavailable_inherited_property() {
    let source = "val value: String = \"OK\"\n\
        open class Carrier(val value: String)\n\
        fun copy(): Carrier = object : Carrier(value) {}\n\
        fun box(): String = copy().value\n";
    common::expect_box_ok_with_stdlib(source, "SuperArgumentTopLevelValue");
}

#[test]
fn an_inferred_outer_getter_precedes_the_unavailable_instance() {
    let source = "open class Carrier(val seed: String) {\n\
        \x20   val value get() = seed\n\
        \x20   fun copy(): Carrier = object : Carrier(value) {}\n\
        }\n\
        fun box(): String = Carrier(\"OK\").copy().seed\n";
    common::expect_box_ok_with_stdlib(source, "SuperArgumentInferredOuterGetter");
}
