//! Runtime behavior of `CallableReferencesToContextual` references: a reference to a declaration with context parameters selects
//! a scope value for each of them where the reference is written and binds it, so the reference's
//! own type shows only the remaining parameters. The language feature exists from Kotlin
//! 2.5.0-RC, whose runtime library also carries the `boundContextArguments` the compiled
//! references hand over; both programs run against that release.

use crate::common;

// PLATFORM-SPECIFIC: these pin the JVM `boundContextArguments` carrier of compiled references, which
// exists only in the 2.5 JVM runtime library; a portable program cannot observe it.
const RELEASE: &str = "2.5.0-RC";

/// Compile `source` with kotlinc 2.5.0-RC and run its `box()` against that release's stdlib. This is
/// the oracle: krusty's result must equal it, and it must be `OK`.
fn kotlinc_release_box_result(source: &str) -> String {
    let work = common::scratch_dir().expect("cannot allocate reference-runtime fixture");
    let main = work.join("Main.kt");
    std::fs::write(&main, source).expect("write reference fixture");
    let output = work.join("out");
    let stdlib = release_stdlib();
    let mut args = vec![main.to_string_lossy().into_owned()];
    args.extend(["-d".to_string(), output.to_string_lossy().into_owned()]);
    args.extend(common::language_directives::kotlinc_args(source));
    args.extend(["-cp".to_string(), stdlib.to_string_lossy().into_owned()]);
    let (code, diagnostics) = common::kotlinc_server::kotlinc_compile_for_release(RELEASE, &args)
        .expect("kotlinc 2.5.0-RC compiler unavailable");
    assert_eq!(
        code, 0,
        "kotlinc {RELEASE} rejected the fixture: {diagnostics}"
    );
    let result = common::run_box(&[], "MainKt", &[output, stdlib, common::jdk_modules()])
        .expect("run kotlinc-built box fixture");
    let _ = std::fs::remove_dir_all(work);
    result
}

fn release_stdlib() -> std::path::PathBuf {
    common::kotlinc_server::kotlinc_lib_for_release(RELEASE)
        .expect("kotlinc 2.5.0-RC")
        .join("kotlin-stdlib.jar")
}

/// krusty's result for `source` against the 2.5.0-RC stdlib, required to equal kotlinc 2.5.0-RC's.
fn expect_same_as_kotlinc_release(source: &str, stem: &str) {
    let reference = kotlinc_release_box_result(source);
    assert_eq!(
        reference, "OK",
        "{stem}: kotlinc {RELEASE} fixture must succeed"
    );
    let jdk = common::jdk_modules();
    let krusty = common::expect_box_run(source, stem, &[release_stdlib()], Some(jdk.as_path()));
    assert_eq!(
        krusty, reference,
        "{stem}: krusty and kotlinc results differ"
    );
}

/// Top-level, member, extension, object and adapted references, with the context arguments chosen
/// at the reference rather than at the call, and a reference whose type is left to inference.
#[test]
fn function_references_bind_their_context_arguments() {
    let source = include_str!("box_regressions/contextParameters/contextualFunctionReferences.kt");
    expect_same_as_kotlinc_release(source, "ContextualFunctionReferences");
}

/// The bound values are the reference's `boundContextArguments`, and equal references bind equal
/// values.
#[test]
fn function_reference_equality_includes_bound_context_arguments() {
    let source = include_str!("contextual_references/contextualFunctionReferenceEquality.kt");
    expect_same_as_kotlinc_release(source, "ContextualFunctionReferenceEquality");
}
