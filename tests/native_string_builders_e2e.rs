//! `StringBuilder` through krusty's own code generator and runtime.
//!
//! A builder is the one text a Kotlin program can WRITE THROUGH, which is what separates it from
//! everything in `native_strings_e2e`: a string is a value and may share its storage with a
//! substring of it, a builder may not share with anything, and the line between the two is
//! `toString`. The programs here mostly test that line -- text handed out before a write must not
//! change when the write lands.
//!
//! Everything a builder is ASKED (`length`, `sb[i]`, iterating it) goes through the same runtime
//! entry points a string's questions do, so these also pin that those entry points read a builder's
//! text and not a string's fields.

use super::common::expect_native_box;

#[test]
fn a_builder_collects_appends_and_answers_them_as_one_string() {
    expect_native_box(
        "fun box(): String {\n\
         \x20   val sb = StringBuilder()\n\
         \x20   sb.append(\"O\")\n\
         \x20   sb.append('K')\n\
         \x20   sb.append(1)\n\
         \x20   sb.append(true)\n\
         \x20   // `append(null)` with no type is AMBIGUOUS -- kotlinc rejects it too, because a bare\n\
         \x20   // `null` fits `String?`, `CharSequence?` and `Any?` alike.\n\
         \x20   sb.append(null as Any?)\n\
         \x20   val text = sb.toString()\n\
         \x20   return if (text == \"OK1truenull\") \"OK\" else \"fail: $text\"\n\
         }\n",
        "BuilderAppends",
        "OK",
    );
}

#[test]
fn append_answers_the_receiver_so_a_chain_is_one_expression() {
    expect_native_box(
        "fun box(): String {\n\
         \x20   val text = StringBuilder().append(\"a\").append(\"b\").append(\"c\").toString()\n\
         \x20   return if (text == \"abc\") \"OK\" else \"fail: $text\"\n\
         }\n",
        "BuilderChain",
        "OK",
    );
}

#[test]
fn a_builders_text_is_a_copy_that_a_later_write_cannot_reach() {
    // The whole reason `toString` copies where `substring` shares. A builder grows in place, so a
    // string that merely VIEWED its storage would change under a program already holding it -- and
    // would change only sometimes, since a write within the current capacity rewrites the bytes and
    // a write past it moves them.
    expect_native_box(
        "fun box(): String {\n\
         \x20   val sb = StringBuilder(\"ab\")\n\
         \x20   val before = sb.toString()\n\
         \x20   sb.append(\"c\")\n\
         \x20   var long = \"\"\n\
         \x20   for (i in 1..64) long += \"x\"\n\
         \x20   sb.append(long)\n\
         \x20   if (before != \"ab\") return \"fail snapshot: $before\"\n\
         \x20   return if (sb.toString().length == 67) \"OK\" else \"fail grown: ${sb.toString().length}\"\n\
         }\n",
        "BuilderSnapshot",
        "OK",
    );
}

#[test]
fn a_builder_answers_the_questions_a_string_answers_about_its_text() {
    // `length` and `sb[i]` are the string entry points, reading a builder's text rather than a
    // string's fields; reading the wrong one would answer from whatever those bytes happened to be.
    expect_native_box(
        "fun box(): String {\n\
         \x20   val sb = StringBuilder(\"hello\")\n\
         \x20   if (sb.length != 5) return \"fail length: ${sb.length}\"\n\
         \x20   if (sb[0] != 'h') return \"fail first\"\n\
         \x20   if (sb[4] != 'o') return \"fail last\"\n\
         \x20   sb.append(\"!\")\n\
         \x20   return if (sb.length == 6 && sb[5] == '!') \"OK\" else \"fail after append\"\n\
         }\n",
        "BuilderQuestions",
        "OK",
    );
}

#[test]
fn append_line_adds_a_newline_and_never_the_platforms_separator() {
    // Kotlin specifies `appendLine` as `\n` on every target, not as the host's line separator.
    expect_native_box(
        "fun box(): String {\n\
         \x20   val sb = StringBuilder()\n\
         \x20   sb.appendLine(\"a\")\n\
         \x20   sb.appendLine()\n\
         \x20   sb.append(\"b\")\n\
         \x20   val text = sb.toString()\n\
         \x20   return if (text == \"a\\n\\nb\") \"OK\" else \"fail: ${text.length}\"\n\
         }\n",
        "BuilderAppendLine",
        "OK",
    );
}

#[test]
fn a_builder_renders_an_element_through_its_own_to_string() {
    // `append` asks the VALUE how it renders, which for a class of the program's own is the
    // override it declared -- the same answer `"$value"` gives and not the identity one.
    expect_native_box(
        "class Point(val x: Int) {\n\
         \x20   override fun toString(): String = \"P($x)\"\n\
         }\n\
         fun box(): String {\n\
         \x20   val text = StringBuilder().append(Point(1)).append(listOf(2, 3)).toString()\n\
         \x20   return if (text == \"P(1)[2, 3]\") \"OK\" else \"fail: $text\"\n\
         }\n",
        "BuilderRenders",
        "OK",
    );
}

#[test]
fn a_builder_holds_its_text_against_the_collector() {
    // The builder's one reference field is the byte array, and the text is traced through it.
    // Enough garbage to force collections while the builder is the only thing keeping it alive.
    expect_native_box(
        "fun box(): String {\n\
         \x20   val sb = StringBuilder()\n\
         \x20   var at = 0\n\
         \x20   while (at < 20000) {\n\
         \x20       sb.append(\"x\")\n\
         \x20       at += 1\n\
         \x20   }\n\
         \x20   return if (sb.length == 20000 && sb[19999] == 'x') \"OK\" else \"fail: ${sb.length}\"\n\
         }\n",
        "BuilderSurvivesCollection",
        "OK",
    );
}

#[test]
fn a_capacity_is_a_hint_and_nothing_a_program_can_see() {
    expect_native_box(
        "fun box(): String {\n\
         \x20   val sb = StringBuilder(2)\n\
         \x20   if (sb.length != 0) return \"fail empty: ${sb.length}\"\n\
         \x20   sb.append(\"abcdef\")\n\
         \x20   return if (sb.toString() == \"abcdef\") \"OK\" else \"fail: $sb\"\n\
         }\n",
        "BuilderCapacity",
        "OK",
    );
}

