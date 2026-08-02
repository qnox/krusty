//! `kotlinx.coroutines.test.runTest { … }` — the coroutines-test entry point — resolves and RUNS
//! end-to-end against the real runtime. `runTest` takes a value-class parameter
//! (`timeout: kotlin.time.Duration`), so kotlinc mangles its JVM name (`runTest-<hash>`) and its
//! `$default` synthetic. Two coordinated fixes made the trailing-lambda call resolve:
//!
//!  1. METADATA ALIGNMENT (`classpath.rs`): `@Metadata` names the value class while the JVM
//!     descriptor carries its erased underlying (`J` for `Duration`). `meta_param_compat` /
//!     `meta_param_exact` resolve the underlying through the platform's value-class knowledge, so
//!     the function keeps its parameter names/defaults instead of falling back to an all-required
//!     signature that no under-applied call can match.
//!  2. DEFAULT-CALL LOOKUP (`symbol_resolver.rs`): the `$default` probe used only the SOURCE
//!     spelling (`runTest$default` — the deprecated unmangled overload). The mangled synthetic is
//!     resolved directly in its base candidate's facade package.
//!
//! Requires the coroutines-test + coroutines jars; skipped when the toolchain is unavailable.
use super::common;

fn run(main: &str) -> Option<String> {
    let jdk = common::jdk_modules()?;
    let sl = common::stdlib_jar()?;
    let corou = common::coroutines_jar()?;
    let corou_test = common::ensure_maven(
        "org.jetbrains.kotlinx",
        "kotlinx-coroutines-test-jvm",
        "1.9.0",
    )?;
    let cp = vec![sl, corou, corou_test, jdk.clone()];
    common::compile_and_run_box(main, "Main", &cp, Some(&jdk))
}

#[test]
fn runtest_trailing_lambda_resolves_and_runs() {
    // `runTest { }` under-applies `runTest(context = …, timeout = …, testBody)`: the call binds
    // the trailing lambda to `testBody` and omits both defaulted leading parameters.
    const SRC: &str = "import kotlinx.coroutines.test.runTest\n\
        fun box(): String {\n\
        \x20 runTest { 21 + 21 }\n\
        \x20 return \"OK\"\n\
        }\n";
    let Some(r) = run(SRC) else { return };
    assert_eq!(r, "OK");
}

#[test]
fn runtest_trailing_lambda_resolves_through_a_star_import() {
    // The default-call lookup goes through the import scope's LEVELS here (not the explicit-import
    // target), pinning that the mangled `$default` probe is import-form independent.
    const SRC: &str = "import kotlinx.coroutines.test.*\n\
        fun box(): String {\n\
        \x20 runTest { 21 + 21 }\n\
        \x20 return \"OK\"\n\
        }\n";
    let Some(r) = run(SRC) else { return };
    assert_eq!(r, "OK");
}
