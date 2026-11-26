//! The bound publication over standard-library types: `StringBuilder` and `String` meet below
//! `T : CharSequence`, and `Int` and `Double` below `T : Number`. kotlinc records the variable's
//! upper type on the intersection, and every later approximation reads it: through another generic
//! call, a collection argument, a local inside a lambda, or a private declaration. An unbounded
//! parameter keeps the common supertype.
use super::common;

const DECLARATIONS: &str = "\
fun <T : CharSequence> pick(vararg values: T): T = values[0]
fun <T> any(vararg values: T): T = values[0]
fun <T : CharSequence> list(vararg values: T): List<T> = values.toList()
fun <T : Number> first(vararg values: T): T = values[0]
fun <T> keep(value: T): T = value
fun picked() = pick(StringBuilder(), \"a\")
fun unbounded() = any(StringBuilder(), \"a\")
fun listed() = list(StringBuilder(), \"a\")
val property = pick(StringBuilder(), \"a\")
fun kept() = keep(pick(StringBuilder(), \"a\"))
fun wrapped() = listOf(pick(StringBuilder(), \"a\"))
fun local() = run { val value = pick(StringBuilder(), \"a\"); value }
fun number() = first(1, 4.5)
private fun hidden() = pick(StringBuilder(), \"a\")
fun hiddenLength() = hidden().length
";

#[test]
fn a_standard_library_intersection_publishes_the_bound() {
    let usage = "\
fun check() {
    val a: Int = picked()
    val b: Int = unbounded()
    val c: Int = listed()
    val d: Int = property
    val e: Int = kept()
    val f: Int = wrapped()
    val g: Int = local()
    val h: Int = number()
}
";
    let sources = [("declarations.kt", DECLARATIONS), ("usage.kt", usage)];
    common::assert_error_blocks_match_kotlinc(&sources, &[]);
    let mismatch = |line: u32, actual: &str| {
        format!("usage.kt:{line}:16: initializer type mismatch: expected 'Int', actual '{actual}'.")
    };
    assert_eq!(
        common::reference_error_blocks(&sources, &[]),
        [
            mismatch(2, "CharSequence"),
            mismatch(3, "Any"),
            mismatch(4, "List<CharSequence>"),
            mismatch(5, "CharSequence"),
            mismatch(6, "CharSequence"),
            mismatch(7, "List<CharSequence>"),
            mismatch(8, "CharSequence"),
            mismatch(9, "Number"),
        ]
    );
}

#[test]
fn a_published_standard_library_bound_is_the_declaration_abi() {
    let usage = "\
fun box(): String {
    val text = picked().length + property.length + kept().length + local().length
    val items = listed().size + wrapped().size + hiddenLength()
    val sum = number().toInt()
    return if (text == 0 && items == 3 && sum == 1) \"OK\" else \"fail $text $items $sum\"
}
";
    let sources = [("declarations.kt", DECLARATIONS), ("box.kt", usage)];
    assert_eq!(common::kotlinc_box_files_result(&sources, "BoxKt"), "OK");
    common::expect_box_ok_files_with_stdlib(&sources, "intersection_upper_bound_abi");
}
