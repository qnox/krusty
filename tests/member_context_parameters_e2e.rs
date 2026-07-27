//! `context(...)` parameters on CLASS MEMBERS (`// LANGUAGE: +ContextParameters`): the member
//! resolves its context arguments from the caller's scope (`with(A(40)) { b.add(2) }`), exactly
//! like the already-supported top-level and local-function forms. Mirrors the corpus
//! `contextParameters/*` files that declare context members inside class bodies.

use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn member_fun_with_context_parameter_resolves_from_with_scope() {
    const SRC: &str = "// LANGUAGE: +ContextParameters\n\
class A(val v: Int)\n\
class B {\n\
    context(a: A)\n\
    fun add(x: Int): Int = a.v + x\n\
}\n\
fun box(): String {\n\
    val b = B()\n\
    val r = with(A(40)) { b.add(2) }\n\
    return if (r == 42) \"OK\" else \"fail: $r\"\n\
}\n";
    assert_eq!(run(SRC).expect("member context param"), "OK");
}

#[test]
fn member_context_from_enclosing_context_function() {
    // The corpus shape: a context MEMBER called from another context-carrying function — the
    // caller's own context parameter satisfies the member's.
    const SRC: &str = "// LANGUAGE: +ContextParameters\n\
class Ctx(val tag: String)\n\
class W {\n\
    context(c: Ctx)\n\
    fun label(x: String): String = c.tag + x\n\
}\n\
context(c: Ctx)\n\
fun call(w: W): String = w.label(\"!\")\n\
fun box(): String {\n\
    val r = with(Ctx(\"OK\")) { call(W()) }\n\
    return if (r == \"OK!\") \"OK\" else \"fail: $r\"\n\
}\n";
    assert_eq!(run(SRC).expect("member context via enclosing"), "OK");
}

#[test]
fn plain_members_unaffected() {
    const SRC: &str = "class C {\n\
    fun f(): Int = 41\n\
    val g: Int = 1\n\
}\n\
fun box(): String = if (C().f() + C().g == 42) \"OK\" else \"fail\"\n";
    assert_eq!(run(SRC).expect("plain members"), "OK");
}
