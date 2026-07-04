//! `break`/`continue` are `Nothing`-typed and may appear as a branch of an `if`/`when` used in value
//! position (`s += if (c) x else break`): the branch lowers to a loop goto, and the branch-merge takes
//! its value from the non-diverging branches. Round-tripped on the JVM.

mod common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn break_as_if_branch_in_value_position() {
    const SRC: &str = "fun test(str: String): String {\n\
    var s = \"\"\n\
    for (i in 1..3) {\n\
        s += if (i < 2) str else break\n\
    }\n\
    return s\n\
}\n\
fun box(): String = if (test(\"OK\") == \"OK\") \"OK\" else \"fail\"\n";
    assert_eq!(run(SRC).expect("break as if-branch value"), "OK");
}

#[test]
fn break_in_if_branch_inside_lambda_is_rejected() {
    // A `break` in an if-branch INSIDE a lambda targets the outer loop — a non-local jump kotlinc itself
    // rejects ("'break' and 'continue' are only allowed inside a loop"). We must not silently compile it.
    const SRC: &str = "fun box(): String {\n\
    for (i in 1..10) {\n\
        val f: () -> Int = { if (i == 5) 1 else break }\n\
        f()\n\
    }\n\
    return \"OK\"\n\
}\n";
    assert!(
        run(SRC).is_none(),
        "non-local break in an if-branch inside a lambda must be rejected"
    );
}

#[test]
fn continue_as_when_branch_in_value_position() {
    const SRC: &str = "fun box(): String {\n\
    var s = \"\"\n\
    for (i in 1..4) {\n\
        s += when (i) {\n\
            2 -> continue\n\
            else -> \"x\"\n\
        }\n\
    }\n\
    return if (s == \"xxx\") \"OK\" else \"fail $s\"\n\
}\n";
    assert_eq!(run(SRC).expect("continue as when-branch value"), "OK");
}
