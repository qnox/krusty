//! An anonymous object an inline function creates around a `crossinline` lambda is copied for
//! each call into the caller, and the copy calls its superclass constructor with the
//! declaration's arguments: a captured value or a value evaluated where the object is created.
//! A lambda that captures values is inlined into the copy beside a value only the superclass
//! call reads, and the copy keeps the lambda's captures in `$<name>$inlined` fields.

// PLATFORM-SPECIFIC: compare the copies' complete JVM class ABI with kotlinc's.
// The whole Kotlin input also runs through the shared portable backend/version inventory.
use super::common;

const SOURCE: &str = include_str!("box_regressions/inlineObjectCopySuperArguments.kt");
const LIFTED: &str =
    include_str!("box_regressions/inlineObjectLiftedLambdaBesideConstructorCapture.kt");

fn assert_reference_abi(file: &str, source: &str) {
    let classes = common::classes_against_kotlinc_module(&[(file, source)]);
    let abi = |classes: &std::collections::BTreeMap<String, Vec<u8>>| {
        classes
            .iter()
            .map(|(name, bytes)| (name.clone(), common::member_table(bytes)))
            .collect::<std::collections::BTreeMap<_, _>>()
    };
    assert_eq!(abi(&classes.krusty), abi(&classes.reference));
}

#[test]
fn a_copied_object_passes_its_superclass_constructor_arguments() {
    common::expect_box_same_as_kotlinc(SOURCE, "InlineObjectCopySuperArguments");
}

#[test]
fn a_copied_object_has_the_reference_compilers_constructor() {
    assert_reference_abi("InlineObjectCopySuperArguments.kt", SOURCE);
}

#[test]
fn a_capturing_lambda_is_inlined_beside_a_constructor_only_capture() {
    common::expect_box_same_as_kotlinc(LIFTED, "InlineObjectLiftedLambdaBesideConstructorCapture");
}

#[test]
fn a_lifted_lambdas_captures_are_the_reference_compilers_fields() {
    assert_reference_abi(
        "InlineObjectLiftedLambdaBesideConstructorCapture.kt",
        LIFTED,
    );
}
