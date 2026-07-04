//! build.722 hh1: the SAME inline HOF (`find`) spliced in BOTH branches of one `if`/`when` expression —
//! `if (c) xs.find { it > 0 } else xs.find { it < 0 }`. The second splice failed with "inline splice
//! failed for …CollectionsKt.find(…)".
//!
//! Root: `emit_when` uses a LINEAR operand-stack counter. The `then` branch left its value on the counter
//! (height 1); the `else` branch — reached by a conditional JUMP, so actually at the pre-branch baseline
//! (height 0) — was emitted while the counter still read 1. A framed inline splice (`find`'s loop body)
//! requires an empty operand baseline, so it bailed. `emit_when` now resets the counter to the branch
//! entry height at each jump-reached branch.
use super::common;

fn run(main: &str) -> Option<String> {
    let jdk = common::jdk_modules()?;
    let sl = common::stdlib_jar()?;
    common::compile_and_run_box(main, "Main", &[sl, jdk.clone()], Some(&jdk))
}

#[test]
fn inline_find_in_both_if_branches() {
    const MAIN: &str = "fun box(): String {\n\
        \x20 val xs = listOf(-1, 2, -3)\n\
        \x20 val c = true\n\
        \x20 val r = if (c) xs.find { it > 0 } else xs.find { it < 0 }\n\
        \x20 return if (r == 2) \"OK\" else \"fail: $r\"\n\
        }\n";
    assert_eq!(run(MAIN).expect("find in both if-branches"), "OK");
}

#[test]
fn inline_find_in_both_when_branches() {
    // The `when`-expression form exercises the same reset for a multi-branch `when`.
    const MAIN: &str = "fun box(): String {\n\
        \x20 val xs = listOf(-1, 2, -3)\n\
        \x20 val k = 1\n\
        \x20 val r = when (k) { 1 -> xs.find { it > 0 }; else -> xs.find { it < 0 } }\n\
        \x20 return if (r == 2) \"OK\" else \"fail: $r\"\n\
        }\n";
    assert_eq!(run(MAIN).expect("find in both when-branches"), "OK");
}

#[test]
fn inline_find_else_branch_taken() {
    // Exercise the else branch actually running (the previously-broken splice site).
    const MAIN: &str = "fun box(): String {\n\
        \x20 val xs = listOf(-1, 2, -3)\n\
        \x20 val c = false\n\
        \x20 val r = if (c) xs.find { it > 0 } else xs.find { it < 0 }\n\
        \x20 return if (r == -1) \"OK\" else \"fail: $r\"\n\
        }\n";
    assert_eq!(run(MAIN).expect("else branch splice"), "OK");
}
