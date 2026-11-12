//! Runtime behavior of `CallableReferencesToContextual` references: a reference to a declaration with context parameters selects
//! a scope value for each of them where the reference is written and binds it, so the reference's
//! own type shows only the remaining parameters. The language feature exists from Kotlin
//! 2.5.0-Beta2, whose runtime library also carries the `boundContextArguments` the compiled
//! references hand over; both programs run against that release.

use crate::common;

fn run_against_beta2(source: &str, stem: &str) -> String {
    let release = "2.5.0-Beta2";
    let stdlib = common::kotlinc_lib_for_release(release)
        .expect("kotlinc 2.5.0-Beta2")
        .join("kotlin-stdlib.jar");
    let jdk = common::jdk_modules();
    common::expect_box_run(source, stem, &[stdlib], Some(jdk.as_path()))
}

/// Top-level, member, extension, object and adapted references, with the context arguments chosen
/// at the reference rather than at the call, and a reference whose type is left to inference.
#[test]
fn function_references_bind_their_context_arguments() {
    let source = include_str!("box_regressions/contextParameters/contextualFunctionReferences.kt");
    assert_eq!(run_against_beta2(source, "ContextualFunctionReferences"), "OK");
}

/// The bound values are the reference's `boundContextArguments`, and equal references bind equal
/// values.
#[test]
fn function_reference_equality_includes_bound_context_arguments() {
    let source =
        include_str!("box_regressions/contextParameters/contextualFunctionReferenceEquality.kt");
    assert_eq!(
        run_against_beta2(source, "ContextualFunctionReferenceEquality"),
        "OK"
    );
}
