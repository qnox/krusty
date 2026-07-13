//! Kotlin's grammar puts `NL*` before the elvis operator (`elvisExpression: … (NL* elvis NL* …)*`), so
//! an expression may continue onto the next line with a leading `?:`:
//! `val y =\n    x\n        ?: default`. `parse_elvis` treated the intervening newline as a statement
//! terminator, so the dangling `?: default` hit `expected an expression`. Now a line-leading `?:`
//! continues the elvis expression. Round-tripped on the JVM.

use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn newline_leading_elvis_continues_the_expression() {
    // The `?:` sits on its own continuation line; `pick(null)` takes the fallback, `pick("got")` the value.
    const SRC: &str = "fun pick(x: String?): String =\n\
    x\n\
        ?: \"default\"\n\
fun box(): String = pick(null) + \"/\" + pick(\"got\")\n";
    assert_eq!(
        run(SRC).expect("newline-leading elvis compiles + runs"),
        "default/got"
    );
}

#[test]
fn newline_leading_elvis_with_throw_fallback() {
    // The common service-layer shape: a call, then `?: throw …` on the next line.
    const SRC: &str = "fun find(x: String?): String =\n\
    x\n\
        ?: throw IllegalStateException(\"missing\")\n\
fun box(): String = find(\"ok\")\n";
    assert_eq!(run(SRC).expect("newline-leading elvis+throw"), "ok");
}
