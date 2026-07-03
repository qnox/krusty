//! A FULLY-QUALIFIED call to a library top-level function with a SYNTACTIC trailing lambda —
//! `kotlinx.coroutines.runBlocking { … }` written without an `import`. kotlinc accepts this (the leading
//! `context` parameter defaults, the lambda binds the trailing `block: suspend CoroutineScope.() -> T`),
//! but krusty rejected it with `unresolved reference 'kotlinx'`: the FQ-call path resolved the leaf
//! function positionally (arity 2 ≠ 1) and never re-typed the trailing lambda against its receiver/suspend
//! SAM, so overload resolution failed and the receiver was (wrongly) evaluated as a value.
//!
//! The checker now re-types the trailing lambda against the callee's block parameter (`CoroutineScope.()
//! -> T`, via the same `top_level_lambda_param_types`/`receivers` shape data the bare-name `import`ed path
//! uses), so resolution binds the result type-parameter (`runBlocking { "x" }: String`), and the lowerer
//! emits the `runBlocking$default(context, block, mask, marker)` `invokestatic`.
use std::path::PathBuf;
mod common;

fn run(main: &str) -> Option<String> {
    let jdk = common::jdk_modules()?;
    let sl = common::stdlib_jar()?;
    let coro =
        PathBuf::from("target/cache/kotlinc/2.4.0/kotlinc/lib/kotlinx-coroutines-core-jvm.jar");
    if !coro.exists() {
        return None;
    }
    common::compile_and_run_box(main, "Main", &[sl, coro, jdk.clone()], Some(&jdk))
}

#[test]
fn fq_runblocking_trailing_lambda_string_result() {
    const MAIN: &str = "fun box(): String = kotlinx.coroutines.runBlocking { \"OK\" }\n";
    assert_eq!(run(MAIN).expect("FQ runBlocking with String result"), "OK");
}

#[test]
fn fq_runblocking_trailing_lambda_int_result() {
    // The block's generic result must bind through the FQ path (`T = Int`, not erased `Any`).
    const MAIN: &str = "fun box(): String {\n\
        \x20 val n = kotlinx.coroutines.runBlocking { 21 + 21 }\n\
        \x20 return if (n == 42) \"OK\" else \"fail: $n\"\n\
        }\n";
    assert_eq!(run(MAIN).expect("FQ runBlocking with Int result"), "OK");
}

#[test]
fn fq_plain_library_call_still_resolves() {
    // Regression guard for the pre-existing plain (no-lambda) FQ call path (`kotlin.math.max`).
    const MAIN: &str = "fun box(): String {\n\
        \x20 val n = kotlin.math.max(3, 7)\n\
        \x20 return if (n == 7) \"OK\" else \"fail: $n\"\n\
        }\n";
    assert_eq!(run(MAIN).expect("FQ kotlin.math.max"), "OK");
}
