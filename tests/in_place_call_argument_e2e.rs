//! An `@InlineOnly` call's argument that calls, computes or branches stays in place.
//!
//! kotlinc moves each argument's code to where the inlined body loads it whenever that code stores
//! no local and jumps nowhere outside itself (`InplaceArgumentsMethodTransformer`). An ordinary
//! function, member, sibling-file or getter invoke, an array or string access, a discarded
//! statement before a value, primitive arithmetic, a comparison, logical connective or `if` whose
//! jumps stay inside the argument, a source `==` and a string template qualify, so
//! `sb.appendLine(word())` keeps `sb` on the operand stack while `word()` runs instead of storing
//! it to a temporary and swapping it back. `Int? == Int` stores its `Int` and an inline call taking
//! a lambda stores its parameters, so both compilers store those arguments.

use super::common;

const SOURCE: &str = "val sb = StringBuilder()\n\
    object C { const val x = 42 }\n\
    fun getC(): C { sb.append('c'); return C }\n\
    fun word(): String = \"w\"\n\
    val computed: String get() = \"g\"\n\
    val lazyValue: String by lazy { \"l\" }\n\
    fun called() { sb.appendLine(word()) }\n\
    fun getter() { sb.appendLine(computed) }\n\
    fun delegated() { sb.appendLine(lazyValue) }\n\
    fun discarded() { sb.appendLine(getC().x) }\n\
    fun arithmetic(s: StringBuilder) { s[0]++ }\n\
    fun compared(a: Int, b: Int) { sb.appendLine(a < b) }\n\
    fun connected(a: Boolean, b: Boolean) { sb.appendLine(a && b) }\n\
    fun chosen(a: Boolean) { sb.appendLine(if (a) word() else computed) }\n\
    fun negated(a: Int) { sb.appendLine(-a * 2) }\n\
    fun indexed(a: IntArray, s: String) { sb.appendLine(a[0] + a.size + s.length) }\n\
    class Talker { fun word(): String = \"t\" }\n\
    fun member(t: Talker) { sb.appendLine(t.word()) }\n\
    fun sibling(s: Speaker) { sb.appendLine(s.word()) }\n\
    fun crossFile() { sb.appendLine(shout()) }\n\
    fun rendered(a: Any) { sb.appendLine(a.toString()) }\n\
    fun equal(a: String, b: String) { sb.appendLine(a == b) }\n\
    fun unequal(a: String?, b: Any?) { sb.appendLine(a != b) }\n\
    fun boxedEqual(a: Int?, b: Int) { sb.appendLine(a == b) }\n\
    fun ieee(a: Double?, b: Double) { sb.appendLine(a == b) }\n\
    fun absent(a: String?) { sb.appendLine(a == null) }\n\
    fun unsignedEqual(a: UInt?, b: UInt) { sb.appendLine(a == b) }\n\
    fun carrierEqual(a: UInt, b: UInt?) { sb.appendLine(a == b) }\n\
    fun template(i: Int, s: String) { sb.appendLine(\"a$i-$s\") }\n\
    fun single(i: Int) { sb.appendLine(\"$i\") }\n\
    fun branchy(c: Boolean) { sb.appendLine(\"x${if (c) 1 else 2}y\") }\n\
    fun counted(l: List<Int>) { sb.appendLine(l.count { it > 1 }) }\n\
    fun box(): String {\n\
    \x20   called(); getter(); delegated(); discarded(); arithmetic(sb)\n\
    \x20   compared(1, 2); connected(true, false); chosen(false); negated(3)\n\
    \x20   indexed(intArrayOf(4), \"ab\")\n\
    \x20   member(Talker()); sibling(Speaker()); crossFile(); rendered(8)\n\
    \x20   equal(\"x\", \"y\"); unequal(null, \"y\"); boxedEqual(5, 5); ieee(1.0, 1.0); absent(null)\n\
    \x20   unsignedEqual(3u, 4u); carrierEqual(3u, 3u); template(1, \"s\"); single(2); branchy(false); counted(listOf(1, 2, 3))\n\
    \x20   val seen = sb.toString()\n\
    \x20   return if (seen == \"x\\ng\\nl\\nc42\\ntrue\\nfalse\\ng\\n-6\\n7\\nt\\nm\\ns\\n8\\nfalse\\ntrue\\ntrue\\ntrue\\ntrue\\nfalse\\ntrue\\na1-s\\n2\\nx2y\\n2\\n\") \"OK\" else seen\n\
    }\n";

/// A sibling file: its class's member is a `Callee::Virtual` and its function a cross-file call.
const SIBLING: &str = "class Speaker { fun word(): String = \"m\" }\n\
    fun shout(): String = \"s\"\n";

#[test]
fn call_arguments_of_an_inline_only_call_stay_in_place_as_kotlinc_does() {
    let classes = common::classes_against_kotlinc_module(&[
        ("Arguments.kt", SOURCE),
        ("Sibling.kt", SIBLING),
    ]);
    assert_eq!(classes.differences(), Vec::<String>::new());
}

#[test]
fn call_arguments_of_an_inline_only_call_run() {
    common::expect_box_ok_files_with_stdlib(
        &[("Arguments.kt", SOURCE), ("Sibling.kt", SIBLING)],
        "in-place call arguments",
    );
}
