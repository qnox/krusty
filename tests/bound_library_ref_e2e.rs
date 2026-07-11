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
