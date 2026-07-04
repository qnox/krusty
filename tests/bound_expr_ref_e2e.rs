//! Bound callable references on an arbitrary EXPRESSION receiver: a bound extension function
//! (`1::foo` → the lifted static `foo(recv)`) and a bound member on a user-class expression receiver
//! (`mk()::dbl`). The receiver is evaluated once and captured. Also covers two OVERLOADED enclosing
//! functions each holding such a reference — their synthesized impls must not clash (named by the ref's
//! unique AST id, not a per-function counter). Round-tripped under `-Xverify:all`.

use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "C")
}

#[test]
fn bound_extension_and_member_refs_on_expressions() {
    const SRC: &str = "fun Int.plusFour() = this + 4\n\
class Box(val n: Int) { fun dbl() = n * 2 }\n\
fun mk(n: Int) = Box(n)\n\
fun call0(f: () -> Int) = f()\n\
fun box(): String {\n\
    if (call0(1::plusFour) != 5) return \"fail ext\"\n\
    if (call0(mk(7)::dbl) != 14) return \"fail member\"\n\
    return \"OK\"\n\
}\n";
    let out = run(SRC).expect("bound expression-receiver refs should compile + run");
    assert_eq!(out, "OK");
}

#[test]
fn bound_refs_in_overloaded_enclosing_functions_do_not_clash() {
    // Two `tag(...)` overloads each capture a fresh `Counter(...)::hit` — the synthesized closure impls
    // must have distinct names despite the shared enclosing-function name.
    const SRC: &str = "var log = \"\"\n\
class Counter(val token: String) { fun hit(): Int { log += token; return 1 } }\n\
fun run0(f: () -> Int): Int = f()\n\
fun tag() { run0(Counter(\"O\")::hit) }\n\
fun tag(unused: Int) { run0(Counter(\"K\")::hit) }\n\
fun box(): String {\n\
    tag()\n\
    tag(0)\n\
    return if (log == \"OK\") \"OK\" else \"fail: \" + log\n\
}\n";
    let out = run(SRC).expect("bound refs in overloaded enclosing fns should not clash");
    assert_eq!(out, "OK");
}
