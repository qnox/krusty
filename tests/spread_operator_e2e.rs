//! The spread operator `*arr` passes an array into a `vararg` parameter. krusty handles the single
//! spread to a top-level vararg function (`foo(*a)`) via `Arrays.copyOf` + `checkcast` — byte-identical
//! to kotlinc; any other shape (mixed spreads, member/library callee, primitive element) cleanly skips.

use super::common;

/// Strict stdlib/JDK run: missing tooling or a rejected source panics with diagnostics.
fn run(src: &str) -> String {
    common::expect_box_run_with_stdlib(src, "Sp")
}

#[test]
fn single_spread_to_toplevel_vararg_runs() {
    let src = r#"
fun foo(vararg s: String): Int = s.size
fun box(): String {
    val a = arrayOf("a", "b", "c")
    return if (foo(*a) == 3) "OK" else "no"
}
"#;
    assert_eq!(run(src), "OK");
}

#[test]
fn spread_forwards_elements_in_order() {
    // The spread array's elements reach the callee unchanged and in order.
    let src = r#"
fun join(vararg s: String): String = s.joinToString("-")
fun box(): String {
    val a = arrayOf("x", "y", "z")
    return if (join(*a) == "x-y-z") "OK" else join(*a)
}
"#;
    assert_eq!(run(src), "OK");
}
