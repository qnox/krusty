//! `tailrec` value-returning functions: tail self-calls are rewritten into a `while(true)` loop, so
//! deep recursion runs without a stack overflow. Round-tripped under `-Xverify:all`.

mod common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "C")
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

#[test]
fn tailrec_unit_returning_runs() {
    // `Unit`-returning `tailrec`: the tail call is a bare statement (not `return f(…)`). Both the
    // bare-tail shape (`{ …; down(n-1) }`) and the if/else shape (recursive in one arm, base in the
    // other) must loop-ize. 1,000,000-deep — plain recursion would StackOverflow.
    const SRC: &str = "val counter: IntArray = intArrayOf(0)\n\
tailrec fun down(n: Int) {\n\
    if (n == 0) return\n\
    counter[0] = counter[0] + 1\n\
    down(n - 1)\n\
}\n\
val pinged: IntArray = intArrayOf(0)\n\
tailrec fun ping(n: Int) {\n\
    if (n == 0) {\n\
        pinged[0] = 1\n\
    } else {\n\
        ping(n - 1)\n\
    }\n\
}\n\
fun box(): String {\n\
    down(1000000)\n\
    if (counter[0] != 1000000) return \"fail down\"\n\
    ping(1000000)\n\
    if (pinged[0] != 1) return \"fail ping\"\n\
    return \"OK\"\n\
}\n";
    let out = run(SRC).expect("Unit-returning tailrec should compile + run without stack overflow");
    assert_eq!(out, "OK");
}
