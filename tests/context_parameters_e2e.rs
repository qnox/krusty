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

#[test]
fn implicit_receiver_member_shadows_top_level() {
    // Inside `with(Scope)`, an unqualified call to a name that is BOTH a member of the receiver and a
    // top-level function binds the MEMBER (kotlinc scoping: the receiver is the nearer scope). Outside
    // the block, the top-level function is called.
    const SRC: &str = "class Scope { fun tag(): String = \"member\" }\n\
        fun tag(): String = \"top-level\"\n\
        fun box(): String {\n\
        \x20 val inside = with(Scope()) { tag() }\n\
        \x20 val outside = tag()\n\
        \x20 return if (inside == \"member\" && outside == \"top-level\") \"OK\" else \"no: \" + inside + \"/\" + outside\n\
        }\n";
    assert_eq!(run(SRC).expect("member shadows top-level"), "OK");
}

#[test]
fn context_top_level_function_maps_reordered_named_arguments() {
    const SRC: &str = "class C\n\
        context(c: C) fun combine(a: String, b: String): String = a + b\n\
        fun box(): String = with(C()) { combine(b = \"K\", a = \"O\") }\n";
    assert_eq!(run(SRC).expect("context named arguments"), "OK");
}

#[test]
fn context_local_function_maps_reordered_named_arguments() {
    const SRC: &str = "class C\n\
        fun box(): String {\n\
        \x20 context(c: C) fun combine(a: String, b: String): String = a + b\n\
        \x20 return with(C()) { combine(b = \"K\", a = \"O\") }\n\
        }\n";
    assert_eq!(
        common::front_end_diagnostics(SRC, &[], None),
        Vec::<String>::new()
    );
    assert_eq!(run(SRC).expect("local context named arguments"), "OK");
}

#[test]
fn context_top_level_function_maps_named_argument_past_default() {
    const SRC: &str = "class C\n\
        context(c: C) fun choose(a: Int = 7, b: String): String = b\n\
        fun box(): String = with(C()) { choose(b = \"OK\") }\n";
    assert_eq!(
        common::front_end_diagnostics(SRC, &[], None),
        Vec::<String>::new()
    );
    assert_eq!(run(SRC).expect("context named argument past default"), "OK");
}

#[test]
fn context_local_function_maps_named_argument_past_default() {
    const SRC: &str = "class C\n\
        fun box(): String {\n\
        \x20 context(c: C) fun choose(a: Int = 7, b: String): String = b\n\
        \x20 return with(C()) { choose(b = \"OK\") }\n\
        }\n";
    assert_eq!(
        common::front_end_diagnostics(SRC, &[], None),
        Vec::<String>::new()
    );
    assert_eq!(
        run(SRC).expect("local context named argument past default"),
        "OK"
    );
}

#[test]
fn context_local_default_cannot_bind_same_named_caller_local() {
    const SRC: &str = "class C\n\
        fun box(): String {\n\
        \x20 val a = 5\n\
        \x20 context(c: C) fun combine(a: Int, b: Int = a): Int = a * 10 + b\n\
        \x20 val actual = with(C()) { combine(a = 1) }\n\
        \x20 return if (actual == 11) \"OK\" else actual.toString()\n\
        }\n";
    let diagnostics = common::front_end_diagnostics(SRC, &[], None);
    assert!(
        diagnostics.iter().any(|message| message.contains(
            "local function default argument that references another parameter is not supported"
        )),
        "{diagnostics:?}"
    );
}

#[test]
fn context_local_positional_default_cannot_bind_same_named_caller_local() {
    const SRC: &str = "class C\n\
        fun box(): String {\n\
        \x20 val a = 5\n\
        \x20 context(c: C) fun combine(a: Int, b: Int = a): Int = a * 10 + b\n\
        \x20 val actual = with(C()) { combine(1) }\n\
        \x20 return if (actual == 11) \"OK\" else actual.toString()\n\
        }\n";
    let diagnostics = common::front_end_diagnostics(SRC, &[], None);
    assert!(
        diagnostics.iter().any(|message| message.contains(
            "local function default argument that references another parameter is not supported"
        )),
        "{diagnostics:?}"
    );
}
