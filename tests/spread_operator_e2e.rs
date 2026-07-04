//! The spread operator `*arr` passes an array into a `vararg` parameter. krusty handles the single
//! spread to a top-level vararg function (`foo(*a)`) via `Arrays.copyOf` + `checkcast` — byte-identical
//! to kotlinc; any other shape (mixed spreads, member/library callee, primitive element) cleanly skips.

mod common;

fn run(src: &str) -> Option<String> {
    let stdlib = common::stdlib_jar()?;
    let jdk = common::jdk_modules()?;
    common::compile_and_run_box(src, "Sp", &[stdlib], Some(&jdk))
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
    if let Some(out) = run(src) {
        assert_eq!(out, "OK");
    }
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
    if let Some(out) = run(src) {
        assert_eq!(out, "OK");
    }
}
