//! Context parameters (`context(a: A) fun f()`): the leading context receivers are supplied IMPLICITLY
//! at the call site — from the enclosing `with`-block receiver, or an in-scope local / enclosing context
//! parameter — rather than positionally. The checker resolves each context parameter to an in-scope
//! source and the lowerer prepends the loaded values (matching kotlinc's leading-value-parameter ABI).
//! Same-file, runnable.
use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn context_from_with_receiver() {
    // The context `a: A` is filled from the enclosing `with(A("OK"))` receiver.
    const SRC: &str = "class A(var x: String) { fun foo(): String = x }\n\
        var result = \"\"\n\
        context(a: A)\n\
        fun test1() { result = a.foo() }\n\
        fun box(): String {\n\
        \x20 with(A(\"OK\")) { test1() }\n\
        \x20 return result\n\
        }\n";
    assert_eq!(run(SRC).expect("context from with receiver"), "OK");
}

#[test]
fn context_from_local_value() {
    // The context `a: A` is filled from an in-scope local of the matching type.
    const SRC: &str = "class A(val x: String) { fun foo(): String = x }\n\
        var result = \"\"\n\
        context(a: A)\n\
        fun test1() { result = a.foo() }\n\
        fun box(): String {\n\
        \x20 val a = A(\"OK\")\n\
        \x20 test1()\n\
        \x20 return result\n\
        }\n";
    assert_eq!(run(SRC).expect("context from local"), "OK");
}

#[test]
fn context_forwarded_through_enclosing_context() {
    // A context parameter is forwarded to a callee that needs the same context.
    const SRC: &str = "class A(val x: String)\n\
        context(a: A) fun leaf(): String = a.x\n\
        context(a: A) fun mid(): String = leaf()\n\
        fun box(): String = with(A(\"OK\")) { mid() }\n";
    assert_eq!(run(SRC).expect("context forwarded"), "OK");
}
