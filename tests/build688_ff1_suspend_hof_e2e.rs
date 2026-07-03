//! build.688 ff1: a TOP-LEVEL `suspend fun` that applies an inline collection HOF (`filter`/`map`) to a
//! suspend call's result (`suspend fun f() = source().filter { it > 0 }`) failed to emit ("not yet
//! supported by the IR backend"); the class-method form already worked (q1, build.642).
//!
//! ROOT: the CPS transform shifts every body value-index up by one to make room for the appended
//! `Continuation` parameter (`shift_locals`). For a top-level function the shift threshold is 0 (no `this`),
//! and `shift_locals` descended into the NESTED lambda body — shifting the `filter`/`map` predicate's own
//! `it` (value-index 0 → 1). The lambda is extracted to a separate method whose parameter stays at index 0,
//! so its now-`GetValue(1)` reads referenced an unallocated slot. A class method (threshold 1) escaped
//! because the lambda's `it` at index 0 was below the threshold. `shift_locals` no longer descends into a
//! `Lambda` body (a separate value-index scope; its captures are closure fields, not enclosing-frame reads).
mod common;

fn run(tag: &str, main: &str) -> Option<String> {
    let jdk = common::jdk_modules()?;
    let sl = common::stdlib_jar()?;
    let corou = common::coroutines_jar()?;
    let libout = common::compile_lib(tag, "package lib\nclass Z\n")?;
    common::compile_and_run_box(main, "Main", &[libout, sl, corou, jdk.clone()], Some(&jdk))
}

#[test]
fn toplevel_suspend_fn_with_inline_filter() {
    const MAIN: &str = "import kotlinx.coroutines.runBlocking\n\
        suspend fun source(): List<Int> = listOf(1, 2, 3)\n\
        suspend fun f(): List<Int> = source().filter { it > 0 }\n\
        fun box(): String = runBlocking { val xs = f(); if (xs.size == 3) \"OK\" else \"fail\" }\n";
    let Some(r) = run("ff1_filter", MAIN) else {
        return;
    };
    assert_eq!(r, "OK");
}

#[test]
fn toplevel_suspend_fn_with_inline_map() {
    const MAIN: &str = "import kotlinx.coroutines.runBlocking\n\
        suspend fun source(): List<Int> = listOf(1, 2, 3)\n\
        suspend fun f(): List<Int> = source().map { it + 1 }\n\
        fun box(): String = runBlocking { val xs = f(); if (xs == listOf(2, 3, 4)) \"OK\" else \"fail\" }\n";
    let Some(r) = run("ff1_map", MAIN) else {
        return;
    };
    assert_eq!(r, "OK");
}

#[test]
fn toplevel_suspend_fn_with_capturing_hof_lambda() {
    // A CAPTURING predicate (`it > k`) — its capture is a closure field, so the shift-skip is still correct.
    const MAIN: &str = "import kotlinx.coroutines.runBlocking\n\
        suspend fun source(): List<Int> = listOf(1, 2, 3)\n\
        suspend fun f(k: Int): List<Int> = source().filter { it > k }\n\
        fun box(): String = runBlocking { val xs = f(1); if (xs == listOf(2, 3)) \"OK\" else \"fail\" }\n";
    let Some(r) = run("ff1_cap", MAIN) else {
        return;
    };
    assert_eq!(r, "OK");
}
