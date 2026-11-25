//! Kotlin's `assert` under each `-Xassertions` mode.
//!
//! kotlinc's JVM default is `legacy`: `assert` is the stdlib's ordinary inline function. Its body
//! evaluates the condition and then reads `kotlin._Assertions.ENABLED`, so a disabled assertion
//! still runs the condition's side effects. The other modes make it a compiler operation: `jvm`
//! guards on the class's assertion status and evaluates nothing when disabled,
//! `always-enable` checks unguarded, and `always-disable` elides it.
//!
//! `compile_and_run_box` launches `java` WITHOUT `-ea`, so assertions are DISABLED at runtime.

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
    // Assertions disabled at runtime (no `-ea`): under `jvm` a false `assert` must be skipped (no
    // throw), and its condition must NOT be evaluated — `side` stays false.
    const SRC: &str = "// ASSERTIONS_MODE: jvm\n\
fun box(): String {\n\
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
    const SRC: &str = "// ASSERTIONS_MODE: jvm\n\
fun box(): String {\n\
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

/// kotlinc's default `legacy` mode inlines the stdlib body: the condition runs even though the
/// assertion is disabled, and a false one does not throw.
#[test]
fn a_default_assert_evaluates_its_condition_like_kotlinc() {
    const SRC: &str = "var side = 0\n\
fun box(): String {\n\
    assert(run { side++; false })\n\
    assert(run { side++; false }) { \"never\" }\n\
    kotlin.assert(run { side++; true })\n\
    return if (side == 3) \"OK\" else \"FAIL: side=$side\"\n\
}\n";
    common::expect_box_same_as_kotlinc(SRC, "Main");
}

/// The default `assert` is the stdlib's inline body, byte for byte: the condition is stored, then
/// `_Assertions.ENABLED` guards the check and the lazily built message.
#[test]
fn a_default_assert_compiles_like_kotlinc() {
    const SRC: &str = "fun check(x: Int, name: String) {\n\
    assert(x > 0)\n\
    assert(x > 1) { \"too small: $name\" }\n\
    kotlin.assert(name.isNotEmpty())\n\
}\n";
    let classes = common::classes_against_kotlinc_module(&[("Check.kt", SRC)]);
    assert_eq!(classes.differences(), Vec::<String>::new());
}

/// Naming kotlinc's default mode explicitly selects the same inline body.
#[test]
fn an_explicit_legacy_assert_compiles_like_kotlinc() {
    const SRC: &str = "// ASSERTIONS_MODE: legacy\n\
fun check(x: Int) {\n\
    assert(x > 0) { \"x=$x\" }\n\
}\n";
    let classes = common::classes_against_kotlinc_module(&[("Check.kt", SRC)]);
    assert_eq!(classes.differences(), Vec::<String>::new());
}
