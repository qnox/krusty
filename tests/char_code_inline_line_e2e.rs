//! `Char.code` is an inline property getter that kotlinc inlines, so its value ends an inlined call
//! (`markLineNumberAfterInlineIfNeeded`): the caller's line is forgotten after it and written again
//! at the next instruction that marks one, such as the `iadd` that consumes it. A constant
//! receiver folds and a non-inline conversion (`toInt()`) does not end an inlined call. A discarded
//! `d.code;` statement ends one too, so a call after it on the same line marks that line again.
//! The end comes from the getter's metadata `inline` flag. `arrayOf` and `emptyArray` are inline
//! too, but kotlinc realizes them as its own intrinsics rather than inlining them, so they end no
//! inlined call.
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

fun nextChar(): Char = 'a'
fun sink() {}

fun discardedCall() {
    nextChar().code; sink()
}

fun discardedRead(d: Char) {
    d.code; sink()
}

fun discardedLine(d: Char) {
    d.code
    sink()
}

fun factories(c: Int): Int {
    return c + arrayOf(1, 2).size + emptyArray<String>().size + 1
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

/// A dependency's public `inline val` getter is inline only by its metadata accessor flag: its
/// classfile method is public, so nothing but the decoded flag makes the provider publish it as
/// inline. The call is then inlined like kotlinc's, and its end writes the line again, instead of
/// an `invokestatic` of the getter.
#[test]
fn a_public_inline_getter_is_inlined_from_its_metadata_flag() {
    common::class_bytes_diff_against_kotlinc(
        "PublicInlineGetter",
        &[(
            "Lib.kt",
            "package lib\n\npublic inline val Int.twice: Int get() = this * 2\n",
        )],
        "import lib.twice\n\nfun sum(c: Int, d: Int): Int {\n    return c + d.twice + 1\n}\n",
        "PublicInlineGetterKt",
        "sum",
    )
    .expect("the reference kotlinc is provisioned")
    .unwrap_or_else(|why| panic!("{why}"));
}
