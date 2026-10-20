//! A `when` subject tested by a range branch is read straight from its `tmp_subject`.
//!
//! fir2ir reads the subject variable afresh in every condition, and each read has the variable's
//! type. kotlinc's `RangeContainsLowering` stores the tested value in a temporary only when it may
//! have side effects (`createTemporaryVariableIfNecessary`), and a plain variable read has none,
//! so `when (c) { in a..b -> … }` compares `tmp_subject` against both bounds directly.
//!
//! krusty typed the subject read of an `in` or `is` condition only through the subject expression
//! it stood for, not on the read itself. Membership saw an untyped value, converted it to the
//! comparison type, and stored that conversion in a second temporary. A subject read once had the
//! extra copy hidden by the bytecode temporaries pass; one read by a second branch kept both
//! slots. Every expectation below is kotlinc's.
use super::common;

const SOURCE: &str = "package subjects\n\
    fun twoRanges(c: Int, a: Int, b: Int) = when (c) { in a..b -> \"in\"; !in 0..10 -> \"far\"; else -> \"out\" }\n\
    fun constantThenRange(c: Int, a: Int, b: Int) = when (c) { 0 -> \"zero\"; in a..b -> \"in\"; else -> \"out\" }\n\
    fun singleRange(c: Int, a: Int, b: Int) = when (c) {\n\
    \x20   in a..b -> \"in\"\n\
    \x20   else -> \"out\"\n\
    }\n\
    fun longRanges(c: Long, a: Long, b: Long) = when (c) { in a..b -> 1; in 0L until 9L -> 2; else -> 3 }\n\
    fun charRanges(c: Char) = when (c) { 'x' -> \"x\"; in 'a'..'f' -> \"hex\"; in '0'..'9' -> \"digit\"; else -> \"other\" }\n\
    fun widened(c: Byte, a: Byte, b: Byte) = when (c) { in a..b -> 1; in 0..10 -> 2; else -> 3 }\n\
    fun mixedWidth(c: Long, a: Int) = when (c) { in 0..a -> 1; in 0L..9L -> 2; else -> 3 }\n\
    fun doubles(c: Double, a: Double) = when (c) { in 0.0..a -> 1; 5.0 -> 2; else -> 3 }\n\
    fun computed(c: Int, a: Int, b: Int): String { val x = c + 1; return when (x) { in a..b -> \"in\"; in 0..3 -> \"low\"; else -> \"out\" } }\n\
    fun called(a: Int, b: Int) = when (number()) { in a..b -> \"in\"; in 0..3 -> \"low\"; else -> \"out\" }\n\
    fun number(): Int = 3\n\
    fun declared(c: Int, a: Int, b: Int) = when (val y = c * 2) { in a..b -> y; in 0..5 -> -y; else -> 0 }\n\
    fun typed(c: Any, a: Int) = when (c) { is String -> c.length; a -> 1; is Int -> c + 1; else -> 0 }\n\
    fun constants(c: Int) = when (c) { 1 -> \"one\"; 2 -> \"two\"; else -> \"other\" }\n\
    fun statement(c: Int, a: Int, b: Int) { when (c) { in a..b -> println(\"in\"); in 0..1 -> println(\"low\"); else -> println(\"out\") } }\n";

#[test]
fn a_range_branch_reads_the_subject_without_a_second_temporary() {
    common::byte_diff_against_kotlinc_cp(
        "WhenSubjectRangeTemporary",
        SOURCE,
        "subjects/WhenSubjectRangeTemporaryKt",
        &[common::stdlib_jar()],
    )
    .expect("reference kotlinc is provisioned")
    .unwrap_or_else(|error| panic!("{error}"));
}

const UNSIGNED: &str = "package subjects\n\
    fun unsignedInt(c: UInt, a: UInt, b: UInt) = when (c) { in a..b -> \"in\"; !in 0u..10u -> \"far\"; else -> \"out\" }\n\
    fun unsignedLong(c: ULong, a: ULong, b: ULong) = when (c) { 0uL -> \"zero\"; in a..b -> \"in\"; else -> \"out\" }\n";

/// The local stores of `method`, in order, as javap spells them.
fn stores(disassembly: &str, method: &str) -> Vec<String> {
    let mut lines = disassembly.lines().map(str::trim);
    lines
        .find(|line| line.contains(&format!(" {method}-")))
        .unwrap_or_else(|| panic!("no method {method} in:\n{disassembly}"));
    lines
        .take_while(|line| !line.is_empty())
        .filter_map(|line| line.split_whitespace().nth(1))
        .filter(|opcode| opcode.ends_with("store") || opcode.contains("store_"))
        .map(|line| line.to_string())
        .collect()
}

/// Unsigned membership takes the same path: the subject is the only value stored. Each method's
/// stores are kotlinc's, taken from `javap -c` of its output (`istore_3`; `lstore 6`, which javap
/// prints with its operand in the next column).
#[test]
fn an_unsigned_range_branch_stores_only_the_subject() {
    let classes = common::compile_in_process(
        UNSIGNED,
        "WhenSubjectUnsignedRange",
        &[common::stdlib_jar()],
        Some(common::jdk_modules().as_path()),
    )
    .expect("krusty compiles the unsigned fixture");
    let (_, bytes) = classes
        .iter()
        .find(|(name, _)| name == "subjects/WhenSubjectUnsignedRangeKt")
        .expect("krusty emits the facade");
    let work = common::scratch_dir().expect("scratch directory");
    let path = work.join("WhenSubjectUnsignedRangeKt.class");
    std::fs::write(&path, bytes).expect("write the facade");
    let disassembly = common::javap(&["-c", "-p", &path.to_string_lossy()]).expect("javap runs");
    let _ = std::fs::remove_dir_all(work);
    assert_eq!(stores(&disassembly, "unsignedInt"), ["istore_3"]);
    assert_eq!(stores(&disassembly, "unsignedLong"), ["lstore"]);
}

#[test]
fn range_branches_over_a_subject_select_the_same_arms() {
    let source = format!(
        "{SOURCE}{}fun box(): String {{\n\
         \x20   val got = listOf(twoRanges(5, 1, 9), twoRanges(-4, 1, 9), twoRanges(10, 1, 9),\n\
         \x20       constantThenRange(0, 1, 9), constantThenRange(3, 1, 9), singleRange(2, 5, 9),\n\
         \x20       longRanges(1L, 1L, 2L).toString(), longRanges(9L, 1L, 2L).toString(),\n\
         \x20       charRanges('x'), charRanges('c'), charRanges('7'), charRanges('?'),\n\
         \x20       widened(3, 1, 2).toString(), mixedWidth(9L, 3).toString(),\n\
         \x20       doubles(5.0, 1.0).toString(), computed(2, 1, 2), called(5, 6), declared(2, 0, 1).toString(),\n\
         \x20       typed(\"ab\", 1).toString(), typed(1, 1).toString(), typed(4, 1).toString(), constants(2),\n\
         \x20       unsignedInt(5u, 1u, 9u), unsignedInt(11u, 1u, 9u), unsignedLong(0uL, 1uL, 2uL),\n\
         \x20       unsignedLong(2uL, 1uL, 2uL)).joinToString(\",\")\n\
         \x20   val want = \"in,far,out,zero,in,out,1,3,x,hex,digit,other,2,2,2,low,low,-4,2,1,5,two,in,far,zero,in\"\n\
         \x20   return if (got == want) \"OK\" else got\n\
         }}\n",
        UNSIGNED.trim_start_matches("package subjects\n"),
    );
    let output = common::compile_and_run_with_stdlib(&source, "WhenSubjectRangeRun");
    assert_eq!(output.as_deref(), Some("OK"));
}
