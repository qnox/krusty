//! A nested `try` combined with a `finally` is supported when the inlined finally is never re-entered:
//! plain nested try + non-diverging finally with no catch. (A diverging finally or a catch in the nest
//! still bails — the inlined finally would run twice — and is rejected by the checker.) Round-tripped.

mod common;

fn run(src: &str) -> Option<String> {
    let sl = common::stdlib_jar()?;
    let jdk = common::jdk_modules()?;
    common::compile_and_run_box(src, "Main", &[sl], Some(&jdk))
}

#[test]
fn nested_finally_runs_each_once_in_order() {
    const SRC: &str = "val sb = StringBuilder()\n\
fun box(): String {\n\
    try {\n\
        try { sb.append(\"A\") } finally { sb.append(\"B\") }\n\
        sb.append(\"C\")\n\
    } finally { sb.append(\"D\") }\n\
    return if (sb.toString() == \"ABCD\") \"OK\" else \"fail $sb\"\n\
}\n";
    assert_eq!(run(SRC).expect("nested non-diverging finally"), "OK");
}

#[test]
fn finally_containing_a_try_is_rejected() {
    // A `finally` whose body contains a `try` is inlined at each exit, duplicating the inner try's
    // exception ranges — krusty rejects it (skip), rather than emit an unverifiable frame.
    const SRC: &str = "val sb = StringBuilder()\n\
fun box(): String {\n\
    try { sb.append(\"X\") } finally {\n\
        try { sb.append(\"Y\") } finally { sb.append(\"Z\") }\n\
    }\n\
    return \"OK\"\n\
}\n";
    assert!(
        run(SRC).is_none(),
        "finally-containing-try must be rejected, not miscompiled"
    );
}
