//! An expression body's implicit `return` marks the body's end line, as kotlinc marks it: a safe
//! call spanning several lines returns on its last line, like a plain call. On a one-line body the
//! mark repeats the line in effect and adds no entry.

use super::common;

const SOURCE: &str = r#"class Box(val v: Int) {
    fun grow(n: Int): Box = Box(v + n)

    fun regrow(other: Box?) = other?.grow(
        v)
}

fun safe(box: Box?) = box?.grow(
    3)

fun plain(box: Box) = box.grow(
    3)

fun trimmed(s: String?) = s?.trim()

fun trimmed2(s: String?) = s?.trim(
)

fun lengthOf(s: String?) = s?.length

fun box(): String {
    val total = safe(Box(1))!!.v + plain(Box(1)).v + Box(2).regrow(Box(1))!!.v
    if (safe(null) != null || trimmed2(null) != null) return "null"
    if (trimmed(" a ") != "a" || lengthOf("ab") != 2) return "trim"
    return if (total == 4 + 4 + 3) "OK" else "total $total"
}
"#;

#[test]
fn a_multi_line_safe_call_body_returns_on_its_last_line_like_kotlinc() {
    common::expect_box_ok_with_stdlib(SOURCE, "SafeCallReturnLine");
    common::assert_classes_identical_to_kotlinc(
        "SafeCallReturnLine",
        SOURCE,
        &["Box", "SafeCallReturnLineKt"],
    );
}
