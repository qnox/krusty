//! Local slots of a block's variables are free again once the block ends, as kotlinc's `FrameMap`
//! makes them: a sibling branch, the next loop, and the statements after a block reuse them.

use super::common;

fn byte_identical(name: &str, src: &str, class: &str) {
    match common::byte_diff_against_kotlinc(name, src, class) {
        None => eprintln!("skip ({name}: reference toolchain unavailable)"),
        Some(Ok(())) => {}
        Some(Err(e)) => panic!("{e}"),
    }
}

fn run(src: &str) -> String {
    common::compile_and_run_with_stdlib(src, "Main").expect("box() ran")
}

/// `a`, `b` and `z` all take slot 3: each branch's local is left at the end of its branch.
#[test]
fn sibling_branches_and_the_next_statement_share_a_slot() {
    byte_identical(
        "slotReuseSiblings",
        "fun siblings(c: Boolean, n: Int): Int {\n\
    var t = 0\n\
    if (c) {\n\
        val a = n + 1\n\
        t = a\n\
    } else {\n\
        val b = n * 2\n\
        t = b\n\
    }\n\
    val z = t + 3\n\
    return z\n\
}\n",
        "SlotReuseSiblingsKt",
    );
}

/// `after` takes the slot of the loop body's `sq`.
#[test]
fn a_loop_body_local_slot_is_reused_after_the_loop() {
    byte_identical(
        "slotReuseLoops",
        "fun loops(n: Int): Int {\n\
    var s = 0\n\
    var i = 0\n\
    while (i < n) {\n\
        val sq = i * i\n\
        s += sq\n\
        i++\n\
    }\n\
    val after = s\n\
    return after\n\
}\n",
        "SlotReuseLoopsKt",
    );
}

/// A `do` body's local stays live through the condition that reads it, and is left after it.
#[test]
fn a_do_while_body_local_lives_through_the_condition() {
    let src = "fun count(n: Int): Int {\n\
    var s = 0\n\
    do {\n\
        val d = s + 1\n\
        s = d\n\
    } while (d < n)\n\
    val after = s * 10\n\
    return after\n\
}\n\
fun box(): String = if (count(4) == 40) \"OK\" else \"FAIL: ${count(4)}\"\n";
    assert_eq!(run(src), "OK");
}

/// Each catch parameter takes the same slot, and the local after the `try` reuses it.
#[test]
fn catch_parameters_and_the_next_local_share_a_slot() {
    let src = "fun divide(n: Int): String {\n\
    var r = \"\"\n\
    try {\n\
        r = (10 / n).toString()\n\
    } catch (e: ArithmeticException) {\n\
        val m = \"div\"\n\
        r = m\n\
    } catch (e: IllegalStateException) {\n\
        r = \"state\"\n\
    }\n\
    val after = r + \"!\"\n\
    return after\n\
}\n\
fun box(): String {\n\
    val results = divide(0) + divide(5)\n\
    return if (results == \"div!2!\") \"OK\" else \"FAIL: $results\"\n\
}\n";
    assert_eq!(run(src), "OK");
}

/// A `finally`'s parked exception slot is handed to the next `try` again, so the locals declared
/// after the first `try`'s block must not be given it: `r` would otherwise share a slot with the
/// throwable the second `finally` parks, and read it back as an `int`.
#[test]
fn a_local_after_a_try_block_does_not_take_the_parked_exception_slot() {
    let src = "fun parked(n: Int): Int {\n\
    if (n > 0) {\n\
        val a = n\n\
        try {\n\
            println(a)\n\
        } finally {\n\
            println(\"first\")\n\
        }\n\
    }\n\
    val v = n + 1\n\
    var r = 0\n\
    try {\n\
        r = 10 / n\n\
    } finally {\n\
        r += v\n\
    }\n\
    return r\n\
}\n\
fun box(): String {\n\
    val thrown = try { parked(0); \"no throw\" } catch (e: ArithmeticException) { \"thrown\" }\n\
    val value = parked(5)\n\
    return if (thrown == \"thrown\" && value == 8) \"OK\" else \"FAIL: $thrown $value\"\n\
}\n";
    assert_eq!(run(src), "OK");
}
