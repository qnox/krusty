//! An under-applied call to a VALUE-CLASS-PARAMETERED builder with a trailing lambda — the shape
//! `kotlinx.coroutines.test.runTest { … }` has, reproduced from a kotlinc-built FIXTURE so the test
//! owns its dependency instead of pinning a third-party jar version.
//!
//! The fixture reproduces the shape exactly, not approximately: a `kotlin.time.Duration` parameter
//! makes kotlinc mangle the JVM name and its defaults synthetic (`runProbe-8Mi8wO0` /
//! `runProbe-8Mi8wO0$default`), and `@JvmMultifileClass` puts the declaration in a facade PART, so
//! resolution has to go through the same paths the real builder needs:
//!
//!  1. METADATA ALIGNMENT (`classpath.rs`): `@Metadata` names the value class while the JVM
//!     descriptor carries its erased underlying (`J` for `Duration`). `meta_param_compat` /
//!     `meta_param_exact` resolve the underlying through the platform's value-class knowledge, so
//!     the function keeps its parameter names/defaults instead of falling back to an all-required
//!     signature that no under-applied call can match.
//!  2. DEFAULT-CALL LOOKUP (`symbol_resolver.rs`): the `$default` probe used only the SOURCE
//!     spelling (the unmangled overload). The mangled synthetic is resolved directly in its base
//!     candidate's facade package.
//!
//! Requires the reference kotlinc + JDK modules; skipped when the toolchain is unavailable.
use super::common;

/// The builder under test, in the two files a multifile facade needs. Value-class parameter,
/// defaulted leading parameter, `suspend` receiver lambda — the real builder's signature, expressed
/// with nothing but the stdlib (`startCoroutine` runs the body).
const FIXTURE: &[(&str, &str)] = &[
    (
        "Probes.kt",
        r#"
            @file:JvmMultifileClass
            @file:JvmName("ProbesKt")

            package fixtures

            import kotlin.coroutines.Continuation
            import kotlin.coroutines.CoroutineContext
            import kotlin.coroutines.EmptyCoroutineContext
            import kotlin.coroutines.startCoroutine
            import kotlin.time.Duration
            import kotlin.time.Duration.Companion.seconds

            class ProbeScope(val budget: Duration)

            fun runProbe(
                context: CoroutineContext = EmptyCoroutineContext,
                timeout: Duration = 60.seconds,
                testBody: suspend ProbeScope.() -> Unit,
            ) {
                val outer = context
                testBody.startCoroutine(
                    ProbeScope(timeout),
                    object : Continuation<Unit> {
                        override val context: CoroutineContext get() = outer
                        override fun resumeWith(result: Result<Unit>) = result.getOrThrow()
                    },
                )
            }
        "#,
    ),
    (
        "ProbesExtra.kt",
        r#"
            @file:JvmMultifileClass
            @file:JvmName("ProbesKt")

            package fixtures

            fun probeMarker(): String = "marker"
        "#,
    ),
];

/// Compile + run `main`'s `box()` against the fixture classpath, asserting `OK`.
///
/// The toolchain lookup is the ONLY skip. A `None` from the compiler is a regression and panics
/// with the front-end diagnostics that explain it — folding both into one `Option` (`let Some(x) =
/// compile(…) else return`) is what let an earlier regression in this file sit green.
fn expect_box_ok(main: &str, case: &str) {
    let Some(libout) = common::compile_libs("runprobe", FIXTURE) else {
        return;
    };
    let Some(stdlib) = common::stdlib_jar() else {
        return;
    };
    let Some(jdk) = common::jdk_modules() else {
        return;
    };
    let classpath = [libout, stdlib];
    let Some(out) = common::compile_and_run_box(main, "Main", &classpath, Some(&jdk)) else {
        panic!(
            "{case}: compile/run returned None: {:?}",
            common::front_end_diagnostics(main, &classpath, Some(&jdk))
        );
    };
    assert_eq!(out, "OK", "{case}");
}

#[test]
fn value_class_builder_trailing_lambda_resolves_and_runs() {
    // `runProbe { }` under-applies `runProbe(context = …, timeout = …, testBody)`: the call binds
    // the trailing lambda to `testBody` and omits both defaulted leading parameters.
    const SRC: &str = "import fixtures.runProbe\n\
        fun box(): String {\n\
        \x20 runProbe { 21 + 21 }\n\
        \x20 return \"OK\"\n\
        }\n";
    expect_box_ok(SRC, "trailing lambda");
}

#[test]
fn value_class_builder_trailing_lambda_resolves_through_a_star_import() {
    // The default-call lookup goes through the import scope's LEVELS here (not the explicit-import
    // target), pinning that the mangled `$default` probe is import-form independent.
    const SRC: &str = "import fixtures.*\n\
        fun box(): String {\n\
        \x20 runProbe { 21 + 21 }\n\
        \x20 return \"OK\"\n\
        }\n";
    expect_box_ok(SRC, "star import");
}
