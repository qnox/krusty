//! The `kotlin.assert` codegen intrinsic. kotlinc does NOT inline the stdlib body; it guards on the
//! per-class JVM assertion flag (`Class.desiredAssertionStatus()`) and, when disabled, does not even
//! evaluate the condition. krusty lowers `assert(cond)` / `assert(cond) { msg }` to that guarded form
//! (or unguarded under `// ASSERTIONS_MODE: always-enable`, elided under `always-disable`).
//!
//! `compile_and_run_box` launches `java` WITHOUT `-ea`, so assertions are DISABLED at runtime — exactly
//! kotlinc's default. A failing `assert` therefore must NOT throw (the guard skips it), and the
//! condition's side effects must NOT run.

mod common;

fn run(src: &str) -> Option<String> {
    let jh = common::java_home()?;
    let sl = common::stdlib_jar()?;
    let jdk = std::path::PathBuf::from(format!("{jh}/lib/modules"));
    common::compile_and_run_box(src, "Main", &[sl], Some(&jdk))
}

#[test]
fn assert_true_compiles_and_runs() {
    const SRC: &str = "fun box(): String { assert(1 + 1 == 2); return \"OK\" }\n";
    assert_eq!(run(SRC).expect("assert(true) compiles + runs"), "OK");
}

#[test]
fn assert_false_skipped_when_disabled() {
    // Assertions disabled at runtime (no `-ea`): a false `assert` must be skipped (no throw), and its
    // condition must NOT be evaluated — `side` stays false.
    const SRC: &str = "fun box(): String {\n\
    var side = false\n\
    assert(run { side = true; false })\n\
    return if (!side) \"OK\" else \"FAIL: condition evaluated\"\n\
}\n";
    assert_eq!(run(SRC).expect("disabled assert skipped"), "OK");
}

#[test]
fn assert_with_message_lambda_compiles() {
    const SRC: &str = "fun box(): String { assert(2 > 1) { \"never\" }; return \"OK\" }\n";
    assert_eq!(run(SRC).expect("assert with message compiles + runs"), "OK");
}

#[test]
fn assert_always_enable_throws_on_false() {
    // `// ASSERTIONS_MODE: always-enable` emits the check UNGUARDED — a false `assert` throws even with
    // runtime assertions disabled.
    const SRC: &str = "// ASSERTIONS_MODE: always-enable\n\
fun no(): Boolean = false\n\
fun box(): String {\n\
    try { assert(no()); return \"FAIL: no throw\" } catch (e: AssertionError) { return \"OK\" }\n\
}\n";
    assert_eq!(run(SRC).expect("always-enable throws"), "OK");
}
