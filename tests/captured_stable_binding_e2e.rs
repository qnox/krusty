//! A captured binding no code can reassign is re-read like the local it copies, as kotlinc does:
//! a range loop in a lambda or a local function compares against the capture instead of a
//! snapshot of it.

use crate::common;

const SOURCE: &str = r#"
fun interface Action {
    fun run(): Int
}

fun until(size: Int): () -> Unit = { for (item in 0 until size) println(item) }

fun through(size: Int): () -> Unit = { for (item in 0..size) println(item) }

fun downTo(size: Int): () -> Unit = { for (item in size downTo 0) println(item) }

fun local(size: Int): Int {
    fun count(): Int {
        var total = 0
        for (item in 0 until size) total += item
        return total
    }
    return count()
}

fun captured(size: Int): Action {
    val limit = size + 1
    return Action {
        var total = 0
        for (item in 0 until limit) total += item
        total
    }
}

fun mutable(size: Int): () -> Int {
    var limit = size
    return {
        var total = 0
        for (item in 0 until limit) {
            total += item
            limit--
        }
        total
    }
}

fun subject(size: Int): () -> String = {
    when (size) {
        0 -> "zero"
        1 -> "one"
        else -> "many"
    }
}

fun box(): String {
    until(2)()
    through(1)()
    downTo(1)()
    if (local(4) != 6) return "local"
    if (captured(3).run() != 6) return "captured"
    if (mutable(4)() != 6) return "mutable"
    if (subject(1)() != "one") return "subject"
    return "OK"
}
"#;

#[test]
fn a_captured_stable_binding_is_re_read_like_kotlinc() {
    common::assert_classes_identical_to_kotlinc(
        "CapturedStableBinding",
        SOURCE,
        &["CapturedStableBindingKt"],
    );
}

#[test]
fn a_captured_stable_binding_keeps_its_semantics() {
    common::expect_box_ok_with_stdlib(SOURCE, "CapturedStableBinding");
}
