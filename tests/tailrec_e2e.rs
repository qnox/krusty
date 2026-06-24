//! `tailrec` value-returning functions: tail self-calls are rewritten into a `while(true)` loop, so
//! deep recursion runs without a stack overflow. Round-tripped under `-Xverify:all`.

mod common;

fn run(src: &str) -> Option<String> {
    let jh = common::java_home()?;
    let sl = common::stdlib_jar()?;
    let jdk = std::path::PathBuf::from(format!("{jh}/lib/modules"));
    common::compile_and_run_box(src, "C", &[sl], Some(&jdk))
}

#[test]
fn tailrec_deep_recursion_runs() {
    // 1,000,000-deep — plain recursion would StackOverflow; the loop transform must run flat.
    const SRC: &str =
        "tailrec fun count(n: Int, acc: Int): Int = if (n == 0) acc else count(n - 1, acc + 1)\n\
tailrec fun findLast(n: Int): Int {\n\
    if (n <= 1) return n\n\
    return findLast(n - 1)\n\
}\n\
fun box(): String {\n\
    if (count(1000000, 0) != 1000000) return \"fail count\"\n\
    if (findLast(1000000) != 1) return \"fail block\"\n\
    return \"OK\"\n\
}\n";
    let out = run(SRC).expect("deep tailrec should compile + run without stack overflow");
    assert_eq!(out, "OK");
}
