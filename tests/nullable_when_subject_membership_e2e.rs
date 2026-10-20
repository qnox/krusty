//! A nullable `when` subject is stored as itself; only a later condition sees it narrowed.
//!
//! `when (c) { null -> …; in 1..3 -> … }` evaluates `c` once into `tmp_subject`, typed `Int?`.
//! The `null` arm compares that value with `null`. Only when it fails does the next condition see
//! the subject as `Int`, and kotlinc unboxes it there, inside that condition.
//!
//! An `in`/`is` condition reuses the subject's own syntax node as its operand. krusty checked that
//! operand in the narrowed flow of the later arm and recorded its narrowed type on the node, which
//! is also the `when`'s subject. The subject was then published as the narrowed `Int`: it was
//! unboxed before any arm ran, and the `null` arm compared a primitive with `null`, which is
//! never true, so the arm was dropped and a null subject threw a NullPointerException. A subject
//! narrowed by a failed `!is Int` was unboxed before its type test the same way.
use super::common;

const SOURCE: &str = "package subjects\n\
    fun range(c: Int?) = when (c) { null -> \"null\"; in 1..3 -> \"in\"; else -> \"out\" }\n\
    fun negated(c: Long?) = when (c) { null -> \"null\"; !in 1L..3L -> \"out\"; else -> \"in\" }\n\
    fun character(c: Char?) = when (c) { null -> \"null\"; in 'a'..'f' -> \"hex\"; else -> \"other\" }\n\
    fun typed(c: Any?) = when (c) { null -> \"null\"; is String -> \"text\"; else -> \"other\" }\n\
    fun collection(c: String?, names: List<String>) = when (c) { null -> \"null\"; in names -> \"known\"; else -> \"unknown\" }\n\
    fun notInt(c: Any?) = when (c) { !is Int -> \"other\"; in 1..3 -> \"in\"; else -> \"out\" }\n\
    class Bag { operator fun contains(x: Int) = x > 2 }\n\
    fun custom(c: Int?, bag: Bag) = when (c) { null -> \"null\"; in bag -> \"in\"; else -> \"out\" }\n";

#[test]
fn a_nullable_subject_is_tested_for_null_before_any_condition_narrows_it() {
    common::byte_diff_against_kotlinc_cp(
        "NullableWhenSubjectMembership",
        SOURCE,
        "subjects/NullableWhenSubjectMembershipKt",
        &[common::stdlib_jar()],
    )
    .expect("reference kotlinc is provisioned")
    .unwrap_or_else(|error| panic!("{error}"));
}

/// An equality arm after the narrowing range arm. Its bytes still differ from kotlinc's, which
/// stores the constant before unboxing the subject for the comparison, so only its behavior is
/// pinned here.
const EQUALITY_AFTER: &str =
    "fun thenConstant(c: Int?) = when (c) { null -> \"null\"; in 1..3 -> \"in\"; 7 -> \"seven\"; else -> \"out\" }\n";

#[test]
fn a_null_subject_takes_the_null_arm() {
    let source = format!(
        "{SOURCE}{EQUALITY_AFTER}fun box(): String {{\n\
         \x20   val names = listOf(\"ann\")\n\
         \x20   val got = listOf(range(null), range(2), range(7), negated(null), negated(2L),\n\
         \x20       negated(9L), character(null), character('c'), character('z'), typed(null),\n\
         \x20       typed(\"s\"), typed(1), collection(null, names), collection(\"ann\", names),\n\
         \x20       collection(\"bob\", names), notInt(null), notInt(\"s\"), notInt(2), notInt(5),\n\
         \x20       thenConstant(null), thenConstant(2), thenConstant(7), thenConstant(9),\n\
         \x20       custom(null, Bag()), custom(5, Bag()), custom(1, Bag())).joinToString(\",\")\n\
         \x20   val want = \"null,in,out,null,in,out,null,hex,other,null,text,other,null,known,unknown,\" +\n\
         \x20       \"other,other,in,out,null,in,seven,out,null,in,out\"\n\
         \x20   return if (got == want) \"OK\" else got\n\
         }}\n"
    );
    let output = common::compile_and_run_with_stdlib(&source, "NullableWhenSubjectRun");
    assert_eq!(output.as_deref(), Some("OK"));
}
