//! `Char.code` is an inline property getter that kotlinc inlines, so its value ends an inlined call
//! (`markLineNumberAfterInlineIfNeeded`): the caller's line is forgotten after it and written again
//! at the next instruction that marks one, such as the `iadd` that consumes it. A constant
//! receiver folds and a non-inline conversion (`toInt()`) does not end an inlined call.
use super::common;

#[test]
fn char_code_ends_an_inlined_call_like_kotlinc() {
    let source = "\
fun invoke(f: () -> Unit) { f() }

fun sum(c: Int, d: Char): Int {
    return c + d.code + 1
}

fun first(c: Int, d: Char): Int {
    return d.code + c
}

fun stored(d: Char): Int {
    val x = d.code
    return x
}

fun converted(c: Int, d: Char): Int {
    return c + d.toInt()
}

fun nested(c: Int, d: Char): Int {
    return c + maxOf(d.code, 2) + 1
}

fun conditions(d: Char): Boolean {
    if (d.code > 3) return true
    return d.code == 4 && d.code < 9
}

fun templates(d: Char, k: Int): String {
    val s = \"x${d.code}y\"
    val q = 'a'.code
    return s + q + (if (k > 0) d.code else k)
}

fun captured(): Int {
    var d = 'a'
    invoke { d++ }
    return 1 + d.code + 2
}
";
    common::byte_diff_against_kotlinc_cp(
        "CharCodeLines",
        source,
        "CharCodeLinesKt",
        &[common::stdlib_jar()],
    )
    .expect("the reference kotlinc is provisioned")
    .unwrap_or_else(|why| panic!("{why}"));
}
