//! A same-module call whose callee takes a `crossinline` lambda is inlined from the callee's
//! compiled body, as kotlinc compiles it. The literal's body is compiled for the object that keeps
//! it, outside its own class: a property it writes goes through the setter, a private one through a
//! synthetic accessor, and its captured outer instance is the copy's `this$0`. A local return from
//! the literal joins its locals' scope at the lambda method's end. A bound reference's receiver is
//! the regenerated object's `receiver$inlined`, and a lambda class the callee's body declares around the
//! parameter is regenerated with it.

use super::common;

fn fixture_sources(source: &str, expected: &[&str]) -> Vec<(String, String)> {
    let (files, java) = krusty::conformance::split_files(source);
    assert!(java.is_empty());
    let files = files
        .into_iter()
        .map(|(stem, text)| (format!("{stem}.kt"), text))
        .collect::<Vec<_>>();
    assert_eq!(
        files
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>(),
        expected
    );
    files
}

const ACCESSORS_AND_RETURNS: &str =
    include_str!("box_regressions/compiledCrossinline/accessorsAndReturns.kt");

// PLATFORM-SPECIFIC: complete JVM class inventory and the original raw-byte comparisons.
#[test]
fn a_crossinline_literal_is_inlined_from_the_compiled_body_like_kotlinc() {
    let files = fixture_sources(ACCESSORS_AND_RETURNS, &["Declaration.kt", "Caller.kt"]);
    let sources = files
        .iter()
        .map(|(name, text)| (name.as_str(), text.as_str()))
        .collect::<Vec<_>>();
    let classes = common::classes_against_kotlinc_module(&sources);
    assert_eq!(
        classes.krusty.keys().collect::<Vec<_>>(),
        classes.reference.keys().collect::<Vec<_>>()
    );
    // The regenerated objects still name the caller's source file and keep the literal's field in
    // their metadata; the classes around them are kotlinc's.
    for name in ["CallerKt", "Container", "DeclarationKt", "Foo", "Named"] {
        assert!(
            classes.krusty.get(name) == classes.reference.get(name),
            "{name} differs from kotlinc's bytes"
        );
    }
}

#[test]
fn a_crossinline_literal_inlined_from_the_compiled_body_runs() {
    let files = fixture_sources(ACCESSORS_AND_RETURNS, &["Declaration.kt", "Caller.kt"]);
    let sources = files
        .iter()
        .map(|(name, text)| (name.as_str(), text.as_str()))
        .collect::<Vec<_>>();
    common::expect_box_ok_files_with_stdlib(&sources, "CallerKt");
}

const WRAPPED: &str = include_str!("box_regressions/compiledCrossinline/wrappedParameter.kt");

#[test]
fn a_lambda_class_around_a_crossinline_parameter_is_regenerated_with_the_literal() {
    let files = fixture_sources(WRAPPED, &["Declaration.kt", "Caller.kt"]);
    let sources = files
        .iter()
        .map(|(name, text)| (name.as_str(), text.as_str()))
        .collect::<Vec<_>>();
    common::expect_box_ok_files_with_stdlib(&sources, "CallerKt");
}

const TOP_LEVEL: &str = include_str!("box_regressions/compiledCrossinline/topLevelProperty.kt");

#[test]
fn a_top_level_property_is_written_through_its_setter_from_code_other_classes_inline() {
    let files = fixture_sources(TOP_LEVEL, &["Declaration.kt", "Caller.kt"]);
    let sources = files
        .iter()
        .map(|(name, text)| (name.as_str(), text.as_str()))
        .collect::<Vec<_>>();
    common::expect_box_ok_files_with_stdlib(&sources, "CallerKt");
}

// kotlinc's `kt22304`: a callable reference and a lambda in an inline function's body are classes
// of that function's scope, which kotlinc makes public (`inInlineFunctionScope`), so a caller in
// another package can construct them. The caller's literal holds another inlined literal of its
// own, which is code of the literal's body, never a method of the caller.
const REFERENCE: &str = include_str!("box_regressions/compiledCrossinline/referenceClasses.kt");

#[test]
fn an_inline_functions_reference_class_is_public_to_a_caller_in_another_package() {
    let files = fixture_sources(REFERENCE, &["Library.kt", "Caller.kt"]);
    let sources = files
        .iter()
        .map(|(name, text)| (name.as_str(), text.as_str()))
        .collect::<Vec<_>>();
    assert_eq!(
        common::kotlinc_box_files_result(&sources, "caller.CallerKt"),
        "OK",
        "kotlinc reference for a reference class constructed from another package",
    );
    common::expect_box_ok_files_with_stdlib(&sources, "caller.CallerKt");
}

// An object's own outer instance and the one an inlined lambda captures are both `this$0`; kotlinc
// renames the object's to `this$0$inline_fun` (`extractParametersMappingAndPatchConstructor`).
// They can be different objects of the same type, or of different types.
const OWN_OUTER_INSTANCE: &str =
    include_str!("box_regressions/compiledCrossinline/ownOuterInstance.kt");

#[test]
fn an_objects_own_outer_instance_is_renamed_beside_a_lambdas_captured_one() {
    let sources = fixture_sources(OWN_OUTER_INSTANCE, &["OwnOuterInstance.kt"]);
    common::expect_box_same_as_kotlinc(&sources[0].1, "OwnOuterInstance");
}

// PLATFORM-SPECIFIC: deliberate collision with JVM-generated outer-instance field names.
// The same object, declaring both the renamed outer instance's name and its first numbered
// candidate as fields of the outer instance's type. kotlinc numbers the rename by how many fields
// share the base name and declares `this$0$inline_fun$1` twice with one descriptor, a class the JVM
// refuses (`ClassFormatError: Duplicate field name`); the copy here skips every name the class
// holds and takes `this$0$inline_fun$2`.
const OWN_OUTER_INSTANCE_NAME_TAKEN: &str = r#"class A(val s: String) {
    inline fun wrap(crossinline f: () -> String): () -> String = object : () -> String {
        val `this$0$inline_fun`: A = A("I")
        val `this$0$inline_fun$1`: A = A("J")
        override fun invoke() = s + f() + `this$0$inline_fun`.s + `this$0$inline_fun$1`.s
    }

    fun go(): String = wrap { s + "K" }()
}

class B(val t: String) {
    fun go(a: A): String = a.wrap { t }()
}

fun box(): String {
    val same = A("O").go()
    if (same != "OOKIJ") return "same type $same"
    val other = B("K").go(A("O"))
    if (other != "OKIJ") return "other type $other"
    return "OK"
}
"#;

#[test]
fn a_renamed_outer_instance_skips_every_field_name_the_object_declares() {
    common::expect_box_ok_with_stdlib(OWN_OUTER_INSTANCE_NAME_TAKEN, "OwnOuterInstanceNameTaken");
}

// `filter`'s compiled body hands `predicate` to the suspend literal it passes to `source`, which
// this compiler materializes rather than inlining into the object it regenerates. Which lowering
// the call takes is decided from that body before anything is inlined
// (`inliner::keeps_lambda_as_value`), never by inlining it and retrying another route.
const KEPT_AS_VALUE: &str = include_str!("box_regressions/compiledCrossinline/keptAsValue.kt");

#[test]
fn a_body_keeping_a_crossinline_lambda_as_a_value_is_routed_before_inlining() {
    let sources = fixture_sources(KEPT_AS_VALUE, &["KeptAsValue.kt"]);
    common::expect_box_same_as_kotlinc(&sources[0].1, "KeptAsValue");
}
