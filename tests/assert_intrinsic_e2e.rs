//! The `kotlin.assert` codegen intrinsic. kotlinc does NOT inline the stdlib body; it guards on the
//! per-class JVM assertion flag (`Class.desiredAssertionStatus()`) and, when disabled, does not even
//! evaluate the condition. krusty lowers `assert(cond)` / `assert(cond) { msg }` to that guarded form
//! (or unguarded under `// ASSERTIONS_MODE: always-enable`, elided under `always-disable`).
//!
//! `compile_and_run_box` launches `java` WITHOUT `-ea`, so assertions are DISABLED at runtime — exactly
//! kotlinc's default. A failing `assert` therefore must NOT throw (the guard skips it), and the
//! condition's side effects must NOT run.

use super::common;

fn run(src: &str) -> String {
    common::expect_box_run_with_stdlib(src, "Main")
}

#[test]
fn assert_true_compiles_and_runs() {
    const SRC: &str = "fun box(): String { assert(1 + 1 == 2); return \"OK\" }\n";
    assert_eq!(run(SRC), "OK");
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
    assert_eq!(run(SRC), "OK");
}

#[test]
fn fully_qualified_assert_uses_the_same_intrinsic() {
    // Intrinsic selection is a property of the resolved callable, not whether its source name was
    // imported or fully qualified. This condition side effect is the discriminating behavior: a
    // normal static call would evaluate `run` before entering stdlib, while the JVM-assert intrinsic
    // guards the entire condition evaluation when assertions are disabled.
    const SRC: &str = "fun box(): String {\n\
    var side = false\n\
    kotlin.assert(run { side = true; false })\n\
    return if (!side) \"OK\" else \"FAIL: condition evaluated\"\n\
}\n";
    assert_eq!(run(SRC), "OK");
}

#[test]
fn assert_with_message_lambda_compiles() {
    const SRC: &str = "fun box(): String { assert(2 > 1) { \"never\" }; return \"OK\" }\n";
    assert_eq!(run(SRC), "OK");
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
    assert_eq!(run(SRC), "OK");
}
