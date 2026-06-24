//! `Pair`/`Triple` constructors (`Pair(a, b)`, `Triple(a, b, c)`) — auto-imported kotlin built-ins
//! constructed directly. Round-tripped under `-Xverify:all`.

mod common;

fn run(src: &str) -> Option<String> {
    let jh = common::java_home()?;
    let sl = common::stdlib_jar()?;
    let jdk = std::path::PathBuf::from(format!("{jh}/lib/modules"));
    common::compile_and_run_box(src, "C", &[sl], Some(&jdk))
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
