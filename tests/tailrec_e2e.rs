//! `tailrec` value-returning functions: tail self-calls are rewritten into a `while(true)` loop, so
//! deep recursion runs without a stack overflow. Round-tripped under `-Xverify:all`.

use super::common;

fn run(src: &str) -> String {
    common::expect_box_run_with_stdlib(src, "C")
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
    let out = run(SRC);
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
    let out = run(SRC);
    assert_eq!(out, "OK");
}

#[test]
fn tailrec_through_a_return_that_is_not_the_last_statement() {
    // A `return` is a tail position wherever it stands: nothing of the function runs after one. In
    // each of these the recursive `return` sits BEFORE the body's final statement, and the loop has
    // to reach it there — 1,000,000-deep, so a call left in place is a StackOverflowError rather
    // than a slightly slower answer.
    const SRC: &str = "tailrec fun early(n: Int): Int {\n\
    if (n > 0) return early(n - 1)\n\
    return 0\n\
}\n\
tailrec fun mixed(n: Int): Int {\n\
    if (n == 10) return 1 + mixed(n - 1)\n\
    if (n > 0) return mixed(n - 1)\n\
    return 0\n\
}\n\
tailrec fun nested(n: Int, acc: Int): Int {\n\
    if (n > 0) {\n\
        val next = acc + 1\n\
        if (next > 0) return nested(n - 1, next)\n\
    }\n\
    return acc\n\
}\n\
tailrec fun chosen(n: Int): Int {\n\
    when {\n\
        n > 0 -> return chosen(n - 1)\n\
        else -> {}\n\
    }\n\
    return n\n\
}\n\
fun box(): String {\n\
    if (early(1000000) != 0) return \"fail early\"\n\
    if (mixed(1000000) != 1) return \"fail mixed\"\n\
    if (nested(1000000, 0) != 1000000) return \"fail nested\"\n\
    if (chosen(1000000) != 0) return \"fail when\"\n\
    return \"OK\"\n\
}\n";
    let out = run(SRC);
    assert_eq!(out, "OK");
}

#[test]
fn tailrec_through_a_return_inside_a_loop() {
    // A LOOP is not a boundary. Kotlin reads `while (…) { … return f(x) }` as a tail call, and the
    // `continue` the rewrite writes carries the synthetic loop's own label — so leaving the inner
    // loop is the rewrite working, not a reason to skip it. A syntax whitelist that refused to
    // descend into loops left both of these recursing.
    const SRC: &str = "tailrec fun walk(n: Int, acc: Int): Int {\n\
    while (true) {\n\
        if (n == 0) return acc\n\
        return walk(n - 1, acc + 1)\n\
    }\n\
}\n\
tailrec fun guardedByFor(n: Int, acc: Int): Int {\n\
    for (once in 0..0) {\n\
        if (n == 0) return acc\n\
        return guardedByFor(n - 1, acc + 1)\n\
    }\n\
    return -1\n\
}\n\
fun box(): String {\n\
    if (walk(1000000, 0) != 1000000) return \"fail while\"\n\
    if (guardedByFor(1000000, 0) != 1000000) return \"fail for\"\n\
    return \"OK\"\n\
}\n";
    let out = run(SRC);
    assert_eq!(out, "OK");
}

#[test]
fn a_return_a_finally_still_follows_is_not_rewritten() {
    // A `try` IS a boundary: its `finally` runs after the `return`, so the call does not leave
    // directly and the frame is still there to come back to. The order is what tells the two
    // apart — real recursion finishes innermost-first, so the trail counts DOWN from the base
    // case, where a loop step would append each `n` on the way in.
    const SRC: &str = "var trail = \"\"\n\
tailrec fun guarded(n: Int): Int {\n\
    try {\n\
        if (n > 0) return guarded(n - 1)\n\
    } finally {\n\
        trail += n.toString()\n\
    }\n\
    return 0\n\
}\n\
fun box(): String {\n\
    guarded(3)\n\
    return if (trail == \"0123\") \"OK\" else \"fail: \" + trail\n\
}\n";
    let out = run(SRC);
    assert_eq!(out, "OK");
}

#[test]
fn a_lambda_in_the_body_keeps_its_own_returns_while_the_body_is_rewritten() {
    // The other boundary, from the outside. `step` is a real function VALUE built inside the
    // `tailrec` body: its `return@step` belongs to the lambda, and rewriting it would turn a value
    // the lambda yields into a step of the enclosing function's loop. The enclosing `return` is
    // this function's and must still become that step — 1,000,000 deep, so the two claims are
    // separable by what the program does rather than by inspecting the IR.
    //
    // Deliberately NOT `run { … }`: an inline call is spliced structurally before this pass, so it
    // leaves no lambda for the walk to stop at and would test nothing.
    const SRC: &str = "tailrec fun viaLambda(n: Int, acc: Int): Int {\n\
    val step: (Int) -> Int = inner@{ x -> if (x < 0) return@inner 0 else x + 1 }\n\
    if (n == 0) return acc\n\
    return viaLambda(n - 1, acc + step(0))\n\
}\n\
fun box(): String {\n\
    val answer = viaLambda(1000000, 0)\n\
    return if (answer == 1000000) \"OK\" else \"fail: \" + answer\n\
}\n";
    let out = run(SRC);
    assert_eq!(out, "OK");
}

#[test]
fn a_call_that_only_looks_like_a_tail_call_still_recurses() {
    // The rewrite must not take a call that has work after it. Each of these would answer wrongly
    // as a loop step: the sum forgets its addition, the guarded one skips its cleanup, and the
    // non-self call is not this function at all. Small depths, because they really do recurse.
    const SRC: &str = "var trail = \"\"\n\
tailrec fun sum(n: Int): Int {\n\
    if (n == 0) return 0\n\
    return n + sum(n - 1)\n\
}\n\
tailrec fun marked(n: Int): Int {\n\
    if (n > 0) {\n\
        val inner = marked(n - 1)\n\
        trail += \"x\"\n\
        return inner\n\
    }\n\
    return 0\n\
}\n\
fun helper(n: Int): Int = n\n\
tailrec fun other(n: Int): Int {\n\
    if (n > 0) return helper(n)\n\
    return 0\n\
}\n\
fun box(): String {\n\
    if (sum(10) != 55) return \"fail sum: \" + sum(10)\n\
    if (marked(3) != 0) return \"fail marked\"\n\
    if (trail != \"xxx\") return \"fail trail: \" + trail\n\
    if (other(5) != 5) return \"fail other\"\n\
    return \"OK\"\n\
}\n";
    let out = run(SRC);
    assert_eq!(out, "OK");
}

/// Every shape above, asked of the REFERENCE compiler as well: krusty's build and kotlinc's build
/// of the same program must answer the same thing.
///
/// This is the assertion the box corpus cannot make for these cases. Its threshold tolerates known
/// failures, so a `tailrec` that silently went back to recursing would keep the lane green; here
/// the two builds are compared directly, and a `StackOverflowError` on one side and an answer on
/// the other is exactly the divergence that would report.
#[test]
fn tailrec_rewriting_agrees_with_kotlinc() {
    const SRC: &str = "var trail = \"\"\n\
tailrec fun early(n: Int): Int {\n\
    if (n > 0) return early(n - 1)\n\
    return 0\n\
}\n\
tailrec fun inLoop(n: Int, acc: Int): Int {\n\
    while (true) {\n\
        if (n == 0) return acc\n\
        return inLoop(n - 1, acc + 1)\n\
    }\n\
}\n\
tailrec fun guarded(n: Int): Int {\n\
    try {\n\
        if (n > 0) return guarded(n - 1)\n\
    } finally {\n\
        trail += n.toString()\n\
    }\n\
    return 0\n\
}\n\
tailrec fun sum(n: Int): Int {\n\
    if (n == 0) return 0\n\
    return n + sum(n - 1)\n\
}\n\
fun box(): String {\n\
    if (early(1000000) != 0) return \"fail early\"\n\
    if (inLoop(1000000, 0) != 1000000) return \"fail loop\"\n\
    guarded(3)\n\
    if (trail != \"0123\") return \"fail finally: \" + trail\n\
    if (sum(10) != 55) return \"fail sum\"\n\
    return \"OK\"\n\
}\n";
    let reference = common::kotlinc_box_result(SRC);
    let krusty = run(SRC);
    assert_eq!(krusty, reference, "krusty and kotlinc disagree");
    assert_eq!(krusty, "OK");
}
