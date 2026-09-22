//! The runtime walking TEXT the program declared.
//!
//! Kotlin's `CharSequence` declares no iterator: its own members are `length` and the indexed
//! read, and every question this runtime answers about text — `withIndex`, the `for` loop, `s[i]`
//! — goes through those two. They knew only the text this runtime builds, a string and a builder,
//! so a `class Chars : CharSequence` declined. Its descriptor now carries a thunk for each, and
//! the two entry points reach them for an object that is neither.
//!
//! Laziness is what these programs check beyond the result: Kotlin's `withIndex` asks for one
//! character at a time, and a counting text can see whether that held.
//!
//! Every expectation is kotlinc's, taken by running the same program under it.

use super::common::{expect_box_ok_with_stdlib, expect_native_box, kotlinc_box_result};

/// Require kotlinc's answer, krusty's JVM answer and the NATIVE answer to agree.
fn every_backend_agrees_with_kotlinc(stem: &str, source: &str) {
    assert_eq!(
        kotlinc_box_result(source),
        "OK",
        "{stem}: unexpected kotlinc result"
    );
    expect_box_ok_with_stdlib(source, stem);
    expect_native_box(source, stem, "OK");
}

/// A `CharSequence` of the program's, walked with its index — and counting, so the walk's shape
/// is visible: one `get` per character and one `length` beyond them.
#[test]
fn a_programs_own_char_sequence_is_walked_with_index() {
    let source = "class Counting(private val s: String) : CharSequence {\n\
         \x20   var lengthCalls = 0\n\
         \x20   var getCalls = 0\n\
         \x20   override val length: Int\n\
         \x20       get() = s.length.also { lengthCalls++ }\n\
         \x20   override fun get(index: Int): Char = s.get(index).also { getCalls++ }\n\
         \x20   override fun subSequence(startIndex: Int, endIndex: Int): CharSequence =\n\
         \x20       s.subSequence(startIndex, endIndex)\n\
         }\n\
         fun box(): String {\n\
         \x20   val cs = Counting(\"abc\")\n\
         \x20   var seen = \"\"\n\
         \x20   for (entry in cs.withIndex()) {\n\
         \x20       seen += \"${entry.index}:${entry.value};\"\n\
         \x20   }\n\
         \x20   if (seen != \"0:a;1:b;2:c;\") return \"fail walk \" + seen\n\
         \x20   if (cs.getCalls != 3) return \"fail get \" + cs.getCalls\n\
         \x20   return \"OK\"\n\
         }\n";
    every_backend_agrees_with_kotlinc("WalkableTextWithIndex", source);
}

/// The indexed read and the length reached through the WIDE type, where the receiver could be a
/// string or one of the program's and no static type tells them apart.
#[test]
fn a_char_sequence_receiver_reaches_either_implementation() {
    let source = "class Chars(private val s: String) : CharSequence {\n\
         \x20   override val length: Int get() = s.length\n\
         \x20   override fun get(index: Int): Char = s[index]\n\
         \x20   override fun subSequence(startIndex: Int, endIndex: Int): CharSequence =\n\
         \x20       s.subSequence(startIndex, endIndex)\n\
         }\n\
         fun read(text: CharSequence): String = \"\" + text[0] + text[text.length - 1]\n\
         fun box(): String {\n\
         \x20   if (read(Chars(\"OK\")) != \"OK\") return \"fail own\"\n\
         \x20   return if (read(\"OK\") == \"OK\") \"OK\" else \"fail string\"\n\
         }\n";
    every_backend_agrees_with_kotlinc("WalkableTextWideReceiver", source);
}

/// A `for` loop over text of the program's, which is the same walk `withIndex` builds on.
#[test]
fn a_for_loop_walks_a_programs_own_text() {
    let source = "class Chars(private val s: String) : CharSequence {\n\
         \x20   override val length: Int get() = s.length\n\
         \x20   override fun get(index: Int): Char = s[index]\n\
         \x20   override fun subSequence(startIndex: Int, endIndex: Int): CharSequence =\n\
         \x20       s.subSequence(startIndex, endIndex)\n\
         }\n\
         fun box(): String {\n\
         \x20   var seen = \"\"\n\
         \x20   for (c in Chars(\"OK\")) seen += c\n\
         \x20   return if (seen == \"OK\") \"OK\" else \"fail \" + seen\n\
         }\n";
    every_backend_agrees_with_kotlinc("WalkableTextForLoop", source);
}
