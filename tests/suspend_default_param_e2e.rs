//! dd1: a classpath `suspend` CLASS method with a defaulted parameter, called with that argument OMITTED
//! (`class S(r) { suspend fun list(f: Filt = Filt()): Int }`, called `s.list()`). The `$default` synthetic
//! for a suspend method carries the `Continuation` as a real trailing parameter of the original method —
//! `list$default(S, Filt, Continuation, int mask, Object marker)` — with the `Continuation` BEFORE the
//! mask/marker. The default-member lowering previously matched only the non-suspend shape (`… int, Object`),
//! and the coroutine pass APPENDED the continuation after the marker, so the mask (an `int`) landed where
//! the `Continuation` was expected (VerifyError).
//!
//! `synthetic_default_member` now also recognises the suspend shape (Continuation before mask/marker) and
//! `append_continuation` INSERTS the continuation value at that position for a `$default` call. Runs
//! end-to-end via `runBlocking`.
mod common;

fn run(tag: &str, lib: &str, main: &str) -> Option<String> {
    let jdk = common::jdk_modules()?;
    let sl = common::stdlib_jar()?;
    let corou = common::coroutines_jar()?;
    let libout = common::compile_lib(tag, lib)?;
    common::compile_and_run_box(main, "Main", &[libout, sl, corou, jdk.clone()], Some(&jdk))
}

const LIB: &str = "package lib\n\
    class Filt(val n: Int = 5)\n\
    class S(val r: Int) { suspend fun list(f: Filt = Filt()): Int = r + f.n }\n";

#[test]
fn suspend_method_omitting_ctor_default_argument() {
    // `s.list()` omits the `Filt = Filt()` default — the `$default` synthetic fills it (`Filt().n == 5`).
    const MAIN: &str = "import lib.S\nimport kotlinx.coroutines.runBlocking\n\
        fun box(): String = runBlocking { val n = S(10).list(); if (n == 15) \"OK\" else \"fail: $n\" }\n";
    let Some(r) = run("dd1_omit", LIB, MAIN) else {
        return;
    };
    assert_eq!(r, "OK");
}

#[test]
fn suspend_method_providing_the_argument_still_works() {
    // Regression guard: providing the argument goes through the plain suspend member call, not `$default`.
    const MAIN: &str = "import lib.S\nimport lib.Filt\nimport kotlinx.coroutines.runBlocking\n\
        fun box(): String = runBlocking { val n = S(10).list(Filt(7)); if (n == 17) \"OK\" else \"fail: $n\" }\n";
    let Some(r) = run("dd1_prov", LIB, MAIN) else {
        return;
    };
    assert_eq!(r, "OK");
}
