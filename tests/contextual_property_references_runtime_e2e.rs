//! Runtime behavior of `CallableReferencesToContextual` property references: the bound context
//! values travel in the carrier's `boundContextArguments`, a field of the 2.5 JVM runtime library,
//! so the program runs against kotlinc 2.5.0-RC's stdlib and must give the result kotlinc's own
//! build of it gives.

use crate::common;

// PLATFORM-SPECIFIC: this pins the JVM `boundContextArguments` carrier of compiled references,
// which exists only in the 2.5 JVM runtime library; a portable program cannot observe it.
const RELEASE: &str = "2.5.0-RC";

fn release_stdlib() -> std::path::PathBuf {
    common::kotlinc_server::kotlinc_lib_for_release(RELEASE)
        .expect("kotlinc 2.5.0-RC")
        .join("kotlin-stdlib.jar")
}

/// Compile `source` with kotlinc 2.5.0-RC and run its `box()` against that release's stdlib.
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

/// Top-level, member and extension properties, read-only and mutable, bound and unbound: krusty's
/// whole result equals kotlinc 2.5.0-RC's, and both are `OK`.
#[test]
fn property_references_bind_their_context_arguments() {
    let source = include_str!("box_regressions/contextParameters/contextualPropertyReferences.kt");
    expect_same_as_kotlinc_release(source, "ContextualPropertyReferences");
}

/// An inline getter is spliced into the carrier's own `get`, which reads its context arguments
/// from `boundContextArguments` ahead of the receiver.
#[test]
fn inline_getters_of_contextual_properties_read_their_bound_contexts() {
    let source =
        include_str!("box_regressions/contextParameters/contextualInlinePropertyReferences.kt");
    expect_same_as_kotlinc_release(source, "ContextualInlinePropertyReferences");
}
