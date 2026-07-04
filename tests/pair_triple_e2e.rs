//! `Pair`/`Triple` constructors (`Pair(a, b)`, `Triple(a, b, c)`) — auto-imported kotlin built-ins
//! constructed directly. Round-tripped under `-Xverify:all`.

mod common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "C")
}

#[test]
fn pair_and_triple_constructors() {
    const SRC: &str = "fun box(): String {\n\
    val p = Pair(1, 2)\n\
    if (p.first != 1 || p.second != 2) return \"fail pair\"\n\
    val t = Triple(\"a\", 2, 3)\n\
    if (t.first != \"a\" || t.second != 2 || t.third != 3) return \"fail triple\"\n\
    return \"OK\"\n\
}\n";
    let out = run(SRC).expect("Pair/Triple constructors should compile + run");
    assert_eq!(out, "OK");
}
