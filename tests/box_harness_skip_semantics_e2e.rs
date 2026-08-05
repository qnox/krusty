//! Guard for the box harness's SKIP semantics.
//!
//! `compile_and_run_box` / `compile_in_process` return `None` for two unrelated reasons: the JVM
//! toolchain isn't provisioned, or the front end rejected the source. The idiom
//! `let Some(r) = run(SRC) else { return };` collapses both into a silent skip, so a genuine
//! compile failure reports as a PASSING test.
//!
//! The strict helpers encode neither condition as optional. The low-level `expect_*` forms require
//! their caller to supply an already-resolved toolchain and panic with front-end diagnostics on a
//! rejection; the `*_with_stdlib` forms resolve the toolchain fail-fast and then delegate to the
//! same contract. These tests pin that distinction so the skip idiom cannot return through the
//! shared harness.

use super::common;

/// Run `f`, returning its panic message.
///
/// The panic hook is deliberately left ALONE. Muting it would swap the process-global hook for the
/// duration of a compile+run, so a concurrent test's failure would print with no message — and if
/// `f` ever stopped panicking, the `unwrap_err` below would unwind before the hook was restored and
/// blank out diagnostics for the rest of the binary. The two expected `thread … panicked` blocks
/// this test prints are the price of that safety.
fn panic_message<F: FnOnce()>(f: F) -> String {
    let err = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).unwrap_err();
    err.downcast_ref::<String>()
        .cloned()
        .or_else(|| err.downcast_ref::<&str>().map(|s| (*s).to_string()))
        .unwrap_or_else(|| "<non-string panic payload>".to_string())
}

/// A source the front end REJECTS must fail the test and name the diagnostic — never skip.
#[test]
fn a_rejected_source_panics_with_its_diagnostics() {
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let src = "fun box(): String = definitelyNotDeclaredAnywhere()\n";
    let diagnostics =
        common::front_end_diagnostics(src, std::slice::from_ref(&stdlib), Some(jdk.as_path()));
    assert!(
        !diagnostics.is_empty(),
        "fixture must actually be rejected by the front end"
    );

    let msg = panic_message(|| {
        common::expect_box_run(
            src,
            "RejectRun",
            std::slice::from_ref(&stdlib),
            Some(jdk.as_path()),
        );
    });
    assert!(
        msg.contains("compile/run returned None"),
        "panic must name the harness step: {msg}"
    );
    assert!(
        msg.contains("definitelyNotDeclaredAnywhere"),
        "panic must carry the front-end diagnostics: {msg}"
    );

    let msg = panic_message(|| {
        common::expect_compile_in_process(
            src,
            "RejectCompile",
            std::slice::from_ref(&stdlib),
            Some(jdk.as_path()),
        );
    });
    assert!(
        msg.contains("definitelyNotDeclaredAnywhere"),
        "the compile-only helper must report diagnostics too: {msg}"
    );
}

/// The stdlib-gated forms return the accepted source directly after fail-fast toolchain lookup.
#[test]
fn an_accepted_source_still_runs_through_the_gated_helpers() {
    let src = "fun box(): String = \"OK\"\n";
    // The strict helpers resolve the toolchain internally and panic on missing inputs or rejection;
    // an impossible optional branch must not reintroduce silent skip semantics.
    let out = common::expect_box_run_with_stdlib(src, "AcceptRun");
    assert_eq!(out, "OK");
    let classes = common::expect_classes_with_stdlib(src, "AcceptCompile");
    assert!(
        classes.iter().any(|(n, _)| n.ends_with("AcceptCompileKt")),
        "emitted classes: {:?}",
        classes.iter().map(|(n, _)| n).collect::<Vec<_>>()
    );
}
