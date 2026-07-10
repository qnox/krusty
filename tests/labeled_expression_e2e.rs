//! A label may prefix ANY expression (`label@ <expr>`), not just a loop or lambda: `l1@ "s"`,
//! `x@ (1L + 2)`, a labeled `if`. On a plain expression the label is a semantic no-op (only a
//! non-local `return@label` would consult it). Before, the parser only accepted `label@` on a loop
//! statement, so a labeled expression failed with `expected ')'` / `expected an expression`. It is
//! now consumed in `parse_prefix` and the labeled expression parses normally. Same-file, runnable.
use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn labeled_parenthesized_expression() {
    // `l1@ "s"` and a labeled `if` inside parentheses (kt454 shape).
    const SRC: &str = "fun box(): String {\n\
        \x20 val s1 = (l1@ \"s\")\n\
        \x20 val s2 = (l2@ if (l3@ true) s1 else \"x\")\n\
        \x20 return if (s2 == \"s\") \"OK\" else \"fail\"\n\
        }\n";
    assert_eq!(run(SRC).expect("labeled parenthesized expression"), "OK");
}

#[test]
fn labeled_expression_as_call_argument() {
    // `x@ (1L + 2)` as a constructor argument (labeledExpressionCast shape).
    const SRC: &str = "class Box<T>(val value: T)\n\
        fun box(): String {\n\
        \x20 val b = Box<Long>(x@ (1L + 2))\n\
        \x20 return if (b.value == 3L) \"OK\" else \"fail\"\n\
        }\n";
    assert_eq!(run(SRC).expect("labeled expression as call argument"), "OK");
}

#[test]
fn labeled_this_receiver_still_resolves() {
    // Guard: `this@Outer` is a labeled RECEIVER, not a labeled expression — it must keep resolving.
    const SRC: &str = "class Outer {\n\
        \x20 val v = \"O\"\n\
        \x20 inner class Inner {\n\
        \x20   fun f(): String = this@Outer.v + \"K\"\n\
        \x20 }\n\
        }\n\
        fun box(): String = Outer().Inner().f().let { if (it == \"OK\") \"OK\" else \"fail\" }\n";
    assert_eq!(run(SRC).expect("labeled this receiver"), "OK");
}
