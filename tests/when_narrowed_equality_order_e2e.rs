//! A `when` subject equality compares the subject as read, as kotlinc's fir2ir does.
//!
//! fir2ir converts `$subj$ == candidate` like any `==` operand: when the smart-cast subject and
//! the candidate have one primitive comparison type, it erases the smart cast. A boxed `Int?`
//! subject narrowed by a `null` or `in` arm therefore meets an `Int` candidate in kotlinc's
//! `PrimitiveToObjectComparison`, which stores the candidate before it unboxes the subject
//! (`aload subj; bipush 7; istore t; invokevirtual intValue; iload t`); the redundant null check
//! after an earlier `null` arm is then removed. krusty unboxed the narrowed subject first and
//! compared primitives, and compared an unnarrowed one with `Intrinsics.areEqual` over a boxed
//! candidate. A floating comparison is fir2ir's `ieee754equals`, whose nullable subject keeps its
//! box (`Intrinsics.areEqual(Ljava/lang/Double;D)Z`). A `Byte` comparison converts both operands
//! to `Int` and keeps the cast; an `Any` subject narrowed to `Double` keeps it too.
use super::common;

const SOURCE: &str = "package subjects\n\
    fun afterRange(c: Int?) = when (c) { null -> \"null\"; in 1..3 -> \"in\"; 7 -> \"seven\"; else -> \"out\" }\n\
    fun afterNull(c: Int?) = when (c) { null -> \"null\"; 7 -> \"seven\"; 8 -> \"eight\"; else -> \"out\" }\n\
    fun several(c: Int?) = when (c) { null -> \"null\"; in 1..3 -> \"in\"; 7, 9 -> \"odd\"; else -> \"out\" }\n\
    fun variable(c: Int?, k: Int) = when (c) { null -> \"null\"; in 1..3 -> \"in\"; k -> \"k\"; else -> \"out\" }\n\
    fun nullableCandidate(c: Int?, k: Int?) = when (c) { null -> \"null\"; in 1..3 -> \"in\"; k -> \"k\"; else -> \"out\" }\n\
    fun long(c: Long?) = when (c) { null -> \"null\"; in 1L..3L -> \"in\"; 7L -> \"seven\"; else -> \"out\" }\n\
    fun character(c: Char?) = when (c) { null -> \"null\"; in 'a'..'c' -> \"abc\"; 'x' -> \"x\"; else -> \"other\" }\n\
    fun first(c: Int?) = when (c) { 7 -> \"seven\"; null -> \"null\"; else -> \"out\" }\n\
    fun firstLong(c: Long?) = when (c) { 7L -> \"seven\"; null -> \"null\"; else -> \"out\" }\n\
    fun firstChar(c: Char?) = when (c) { 'x' -> \"x\"; null -> \"null\"; else -> \"other\" }\n\
    fun notInt(c: Any) = when (c) { !is Int -> \"other\"; 7 -> \"seven\"; else -> \"out\" }\n\
    fun read(): Int? = 7\n\
    fun declared() = when (val s = read()) { null -> \"null\"; 7 -> \"seven\"; else -> \"out$s\" }\n\
    fun narrowedByte(b: Byte?) = when (b) { null -> \"null\"; 7.toByte() -> \"seven\"; else -> \"out\" }\n\
    fun double(d: Double?) = when (d) { null -> \"null\"; 1.0 -> \"one\"; else -> \"out\" }\n\
    fun firstDouble(d: Double?) = when (d) { 1.0 -> \"one\"; null -> \"null\"; else -> \"out\" }\n\
    fun float(f: Float?) = when (f) { null -> \"null\"; 1.0f -> \"one\"; else -> \"out\" }\n\
    fun notDouble(d: Any) = when (d) { !is Double -> \"other\"; 1.0 -> \"one\"; else -> \"out\" }\n";

#[test]
fn a_narrowed_subject_is_compared_as_read() {
    common::byte_diff_against_kotlinc_cp(
        "WhenNarrowedEqualityOrder",
        SOURCE,
        "subjects/WhenNarrowedEqualityOrderKt",
        &[common::stdlib_jar()],
    )
    .expect("reference kotlinc is provisioned")
    .unwrap_or_else(|error| panic!("{error}"));
}

#[test]
fn a_subject_compared_as_read_selects_the_same_arms() {
    let source = format!(
        "{SOURCE}fun box(): String {{\n\
         \x20   val got = listOf(afterRange(null), afterRange(2), afterRange(7), afterRange(9),\n\
         \x20       afterNull(null), afterNull(7), afterNull(8), afterNull(1), several(9), several(4),\n\
         \x20       variable(5, 5), variable(6, 5), nullableCandidate(5, 5), nullableCandidate(5, null),\n\
         \x20       long(7L), long(8L), character('x'), character('b'), character(null),\n\
         \x20       first(7), first(null), first(8), firstLong(7L), firstLong(null), firstChar('x'),\n\
         \x20       firstChar('y'), notInt(\"s\"), notInt(7), notInt(8), declared(),\n\
         \x20       narrowedByte(7), narrowedByte(null), double(1.0), double(-0.0), double(null),\n\
         \x20       firstDouble(1.0), firstDouble(null), float(1.0f), float(2.0f), notDouble(1.0),\n\
         \x20       notDouble(1)).joinToString(\",\")\n\
         \x20   val want = \"null,in,seven,out,null,seven,eight,out,odd,out,k,out,k,out,seven,out,x,abc,\" +\n\
         \x20       \"null,seven,null,out,seven,null,x,other,other,seven,out,seven,seven,null,one,out,\" +\n\
         \x20       \"null,one,null,one,out,one,other\"\n\
         \x20   return if (got == want) \"OK\" else got\n\
         }}\n"
    );
    let output = common::compile_and_run_with_stdlib(&source, "WhenNarrowedEqualityOrderRun");
    assert_eq!(output.as_deref(), Some("OK"));
}
