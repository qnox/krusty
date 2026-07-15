//! A bare call resolved against a flow-narrowed implicit receiver: `if (this is B) foo()` inside a
//! member (or extension) body, where `foo` is a member of the subtype `B`, not the declared receiver
//! `A`. The checker resolves the call through `this_narrow` and records the narrowing; the lowerer
//! `checkcast`s `this` to `B` before dispatching. Same-file, runs on the JVM.
use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn narrowed_this_resolves_subtype_member_call() {
    // `A.test()` narrows `this` (declared `A`) to `B` via `this is B`, then calls `B`'s own `foo()`.
    const SRC: &str = "\
open class A {\n\
    fun test(): Int = if (this is B) foo() else 0\n\
}\n\
class B : A() {\n\
    fun foo() = 42\n\
}\n\
fun box(): String {\n\
    if (B().test() != 42) return \"f1\"\n\
    if (A().test() != 0) return \"f2\"\n\
    return \"OK\"\n\
}\n";
    assert_eq!(run(SRC).expect("narrowed this member call"), "OK");
}

#[test]
fn narrowed_this_call_passes_arguments() {
    // Regression companion: the narrowed dispatch must forward arguments and the result type.
    const SRC: &str = "\
open class A {\n\
    fun pick(): Int = if (this is B) plus(40, 2) else -1\n\
}\n\
class B : A() {\n\
    fun plus(x: Int, y: Int) = x + y\n\
}\n\
fun box(): String = if (B().pick() == 42) \"OK\" else \"FAIL\"\n";
    assert_eq!(run(SRC).expect("narrowed this call with args"), "OK");
}
