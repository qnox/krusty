//! A `Nothing`-returning function CALL (not `throw`/`return`) used as a branch of an `if`/`when`
//! statement must terminate that path — kotlinc discards the physical `Void` the call leaves and throws
//! `KotlinNothingValueException`. Without that, the diverging branch leaks a `Void` into the merge frame
//! (VerifyError: inconsistent stackmap frames). Round-tripped on the JVM.

use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn nothing_call_in_else_branch() {
    const SRC: &str = "var flag = true\n\
fun exit(): Nothing = throw RuntimeException(\"boom\")\n\
fun box(): String {\n\
    var a: String\n\
    if (flag) { a = \"OK\" } else { exit() }\n\
    return a\n\
}\n";
    assert_eq!(run(SRC).expect("Nothing call in else branch"), "OK");
}

#[test]
fn nothing_call_in_if_expression_value() {
    const SRC: &str = "fun fail(): Nothing = throw RuntimeException(\"x\")\n\
fun pick(b: Boolean): String {\n\
    val s = if (b) \"yes\" else fail()\n\
    return s\n\
}\n\
fun box(): String = if (pick(true) == \"yes\") \"OK\" else \"fail\"\n";
    assert_eq!(run(SRC).expect("Nothing call in if-expression value"), "OK");
}

/// The same rule for `null!!`, whose value type is `Nothing` without being a call.
///
/// The assertion always throws, so what follows is unreachable — but it physically leaves the
/// asserted REFERENCE on the stack and falls through. Where the sibling branch yields a primitive
/// the merge frame gets a reference where an `int` belongs, and the verifier rejects the method
/// outright (`Type null is not assignable to integer`). A reference sibling hid this: `null` merges
/// with `String` perfectly well, which is why only the primitive cases were red.
fn run_both(src: &str, what: &str) {
    assert_eq!(run(src).expect(what), "OK");
    let Some(out) = common::kotlinc_library(src) else {
        return;
    };
    assert_eq!(
        common::run_box(&[], "LibKt", &[out, common::stdlib_jar()]).as_deref(),
        Some("OK"),
        "{what}: reference compiler"
    );
}

#[test]
fn not_null_assertion_of_null_terminates_a_boolean_branch() {
    const SRC: &str = "fun f(a: Int): Boolean = if (a > 0) true else null!!\n\
fun box(): String {\n\
    if (!f(1)) return \"fail: taken branch\"\n\
    val thrown = try { f(0); \"none\" } catch (e: NullPointerException) { \"npe\" }\n\
    return if (thrown == \"npe\") \"OK\" else \"fail: $thrown\"\n\
}\n";
    run_both(SRC, "null!! opposite a Boolean");
}

#[test]
fn not_null_assertion_of_null_terminates_every_primitive_width() {
    const SRC: &str = "fun i(a: Int) = if (a > 0) 7 else null!!\n\
fun l(a: Int) = if (a > 0) 7L else null!!\n\
fun d(a: Int) = if (a > 0) 7.5 else null!!\n\
fun c(a: Int) = if (a > 0) 'x' else null!!\n\
fun box(): String {\n\
    if (i(1) != 7) return \"fail: Int\"\n\
    if (l(1) != 7L) return \"fail: Long\"\n\
    if (d(1) != 7.5) return \"fail: Double\"\n\
    if (c(1) != 'x') return \"fail: Char\"\n\
    return \"OK\"\n\
}\n";
    run_both(SRC, "null!! opposite each primitive width");
}

#[test]
fn not_null_assertion_of_null_terminates_a_when_arm() {
    const SRC: &str = "fun f(a: Int): Int = when {\n\
    a > 0 -> 1\n\
    a < 0 -> -1\n\
    else -> null!!\n\
}\n\
fun box(): String {\n\
    if (f(3) != 1) return \"fail: positive\"\n\
    if (f(-3) != -1) return \"fail: negative\"\n\
    return \"OK\"\n\
}\n";
    run_both(SRC, "null!! in a when arm");
}

#[test]
fn a_reference_sibling_of_a_terminated_assertion_still_merges() {
    const SRC: &str = "fun f(a: Int): String = if (a > 0) \"y\" else null!!\n\
fun box(): String = if (f(1) == \"y\") \"OK\" else \"fail\"\n";
    run_both(SRC, "null!! opposite a reference");
}
