//! Bound callable reference on a LIBRARY-type receiver (`"KOTLIN"::get`): the receiver is evaluated
//! once and captured; `invoke(args)` dispatches the resolved classpath instance method (here
//! `java/lang/String.charAt(I)C`) on it. Before, the checker rejected any bound ref whose member
//! wasn't a user-class method / same-module extension ("callable references are not supported").
//! Same-file, runnable.
use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn bound_string_get_ref() {
    const SRC: &str = "fun box(): String {\n\
        \x20 val f = \"KOTLIN\"::get\n\
        \x20 return \"${f(1)}${f(0)}\"\n\
        }\n";
    assert_eq!(run(SRC).expect("bound string::get ref"), "OK");
}

// A bound PROPERTY reference on a library receiver (`"kotlin"::length`) lowers to a
// `PropertyReference0Impl` whose `get()` dispatches the classpath getter (`String.length()`), even
// though a same-named method exists — the metadata classifies `length` as a property.
#[test]
fn bound_string_length_prop_ref() {
    const SRC: &str = "fun box(): String {\n\
        \x20 val f = \"kotlin\"::length\n\
        \x20 return if (f.get() == 6) \"OK\" else \"Fail: ${f.get()}\"\n\
        }\n";
    assert_eq!(run(SRC).expect("bound string::length prop ref"), "OK");
}

// A bound PROPERTY reference on an ARBITRARY-EXPRESSION receiver of a USER class (`A(..)::p`): the
// receiver expression is evaluated once and captured, then `get()` reads the property. Bound METHOD
// refs on such a receiver already worked; the property form did not.
#[test]
fn bound_user_prop_ref_expr_receiver() {
    const SRC: &str = "class A(val p: String)\n\
        fun box(): String {\n\
        \x20 val f = A(\"OK\")::p\n\
        \x20 return f.get()\n\
        }\n";
    assert_eq!(run(SRC).expect("bound user prop ref, expr receiver"), "OK");
}
