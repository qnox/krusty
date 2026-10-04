//! `kotlin.text`'s questions about a string, answered by the runtime.
//!
//! A krusty string holds UTF-8 and Kotlin counts UTF-16 units, so none of these is a question the
//! generator can answer by rewriting it into something else: each needs a walk of the encoding,
//! which is the runtime's job and where `length` and `s[i]` already live.
//!
//! Three of them — `startsWith`, `endsWith`, `contains` — carry Kotlin's `ignoreCase` flag. The
//! runtime answers only the case-SENSITIVE form: case folding is a question about Unicode, not
//! about text, and the runtime holds no case table. The two providers hand the default over
//! differently — a klib call materializes it as a constant `false`, a jar call leaves the argument
//! out — so the call site reads its ARGUMENTS rather than the signature, and both are one answer.
//!
//! Every expectation is kotlinc's, taken by running the same program under it.

use super::common::{expect_box_ok_with_stdlib, expect_native_box, expect_native_decline};

/// Emptiness and blankness, over text that is neither, empty, and whitespace only.
#[test]
fn a_string_answers_whether_it_is_empty_and_whether_it_is_blank() {
    let source = "fun box(): String {\n\
         \x20   if (\"Hi\".isEmpty()) return \"fail isEmpty\"\n\
         \x20   if (!\"\".isEmpty()) return \"fail empty isEmpty\"\n\
         \x20   if (!\"Hi\".isNotEmpty()) return \"fail isNotEmpty\"\n\
         \x20   if (\"\".isNotEmpty()) return \"fail empty isNotEmpty\"\n\
         \x20   if (\"Hi\".isBlank()) return \"fail isBlank\"\n\
         \x20   if (!\"\".isBlank()) return \"fail empty isBlank\"\n\
         \x20   if (!\" \\t\\n \".isBlank()) return \"fail whitespace isBlank\"\n\
         \x20   if (!\"Hi\".isNotBlank()) return \"fail isNotBlank\"\n\
         \x20   if (\"  \".isNotBlank()) return \"fail whitespace isNotBlank\"\n\
         \x20   return \"OK\"\n\
         }\n";
    expect_box_ok_with_stdlib(source, "StringEmptiness");
    expect_native_box(source, "StringEmptiness", "OK");
}

/// Trimming at either end and at both, including text that is whitespace all the way through.
#[test]
fn a_string_trims_its_whitespace_at_either_end() {
    let source = "fun box(): String {\n\
         \x20   if (\"  ab  \".trim() != \"ab\") return \"fail trim\"\n\
         \x20   if (\"  ab  \".trimStart() != \"ab  \") return \"fail trimStart\"\n\
         \x20   if (\"  ab  \".trimEnd() != \"  ab\") return \"fail trimEnd\"\n\
         \x20   if (\"ab\".trim() != \"ab\") return \"fail nothing to trim\"\n\
         \x20   if (\" \\t\\r\\n \".trim() != \"\") return \"fail all whitespace\"\n\
         \x20   if (\"\".trim() != \"\") return \"fail empty\"\n\
         \x20   // A non-breaking space is whitespace to Kotlin, which takes the UNION of Java's\n\
         \x20   // `isWhitespace` and `isSpaceChar`.\n\
         \x20   if (\"\\u00A0x\\u00A0\".trim() != \"x\") return \"fail non-breaking\"\n\
         \x20   return \"OK\"\n\
         }\n";
    expect_box_ok_with_stdlib(source, "StringTrim");
    expect_native_box(source, "StringTrim", "OK");
}

/// `startsWith`, `endsWith` and `contains`, written both ways the flag can be left at its default.
#[test]
fn a_string_answers_where_another_one_sits_inside_it() {
    let source = "fun box(): String {\n\
         \x20   val text = \"Hello, world\"\n\
         \x20   if (!text.startsWith(\"Hello\")) return \"fail startsWith\"\n\
         \x20   if (text.startsWith(\"world\")) return \"fail startsWith miss\"\n\
         \x20   if (!text.startsWith(\"Hello\", ignoreCase = false)) return \"fail startsWith flag\"\n\
         \x20   if (!text.endsWith(\"world\")) return \"fail endsWith\"\n\
         \x20   if (text.endsWith(\"Hello\")) return \"fail endsWith miss\"\n\
         \x20   if (!text.endsWith(\"world\", ignoreCase = false)) return \"fail endsWith flag\"\n\
         \x20   if (!text.contains(\"o, w\")) return \"fail contains\"\n\
         \x20   if (text.contains(\"World\")) return \"fail contains miss\"\n\
         \x20   if (!(\"lo, \" in text)) return \"fail in\"\n\
         \x20   // The empty text is a prefix, a suffix and a part of everything.\n\
         \x20   if (!text.startsWith(\"\") || !text.endsWith(\"\") || !text.contains(\"\"))\n\
         \x20       return \"fail empty operand\"\n\
         \x20   // A longer operand cannot be any of the three.\n\
         \x20   if (\"ab\".startsWith(\"abc\") || \"ab\".endsWith(\"abc\") || \"ab\".contains(\"abc\"))\n\
         \x20       return \"fail longer operand\"\n\
         \x20   return \"OK\"\n\
         }\n";
    expect_box_ok_with_stdlib(source, "StringSearch");
    expect_native_box(source, "StringSearch", "OK");
}

/// `repeat`, `reversed`, `first` and `last`, with text above the BMP kept whole.
#[test]
fn a_string_repeats_reverses_and_answers_the_character_at_either_end() {
    let source = "fun box(): String {\n\
         \x20   if (\"ab\".repeat(3) != \"ababab\") return \"fail repeat\"\n\
         \x20   if (\"ab\".repeat(0) != \"\") return \"fail repeat none\"\n\
         \x20   if (\"abc\".reversed() != \"cba\") return \"fail reversed\"\n\
         \x20   if (\"\".reversed() != \"\") return \"fail reversed empty\"\n\
         \x20   // Reversal is by CHARACTER: a character above U+FFFF is two UTF-16 units and must\n\
         \x20   // not come apart.\n\
         \x20   if (\"a\\uD83D\\uDE00b\".reversed() != \"b\\uD83D\\uDE00a\") return \"fail surrogate\"\n\
         \x20   if (\"abc\".first() != 'a') return \"fail first\"\n\
         \x20   if (\"abc\".last() != 'c') return \"fail last\"\n\
         \x20   return \"OK\"\n\
         }\n";
    expect_box_ok_with_stdlib(source, "StringShapes");
    expect_native_box(source, "StringShapes", "OK");
}

/// The failures Kotlin raises: an empty text has no first character, and `repeat` takes no
/// negative count.
#[test]
fn an_empty_string_has_no_first_character_and_repeat_takes_no_negative_count() {
    let source = "fun box(): String {\n\
         \x20   var raised = 0\n\
         \x20   try {\n\
         \x20       \"\".first()\n\
         \x20   } catch (e: NoSuchElementException) {\n\
         \x20       raised++\n\
         \x20   }\n\
         \x20   try {\n\
         \x20       \"\".last()\n\
         \x20   } catch (e: NoSuchElementException) {\n\
         \x20       raised++\n\
         \x20   }\n\
         \x20   try {\n\
         \x20       \"a\".repeat(-1)\n\
         \x20   } catch (e: IllegalArgumentException) {\n\
         \x20       raised++\n\
         \x20   }\n\
         \x20   return if (raised == 3) \"OK\" else \"fail \" + raised\n\
         }\n";
    expect_box_ok_with_stdlib(source, "StringFailures");
    expect_native_box(source, "StringFailures", "OK");
}

/// A builder is a `CharSequence` too, and the same questions reach it.
#[test]
fn a_string_builder_answers_the_same_questions_about_its_text() {
    let source = "fun box(): String {\n\
         \x20   val builder = StringBuilder()\n\
         \x20   if (!builder.isEmpty()) return \"fail empty builder\"\n\
         \x20   builder.append(\"  ab  \")\n\
         \x20   if (builder.isEmpty()) return \"fail filled builder\"\n\
         \x20   if (builder.trim().toString() != \"ab\") return \"fail builder trim\"\n\
         \x20   if (builder.first() != ' ') return \"fail builder first\"\n\
         \x20   // The trimmed text must survive the builder growing past its storage.\n\
         \x20   val trimmed = builder.trim().toString()\n\
         \x20   builder.append(\"cdefghijklmnopqrstuvwxyz\")\n\
         \x20   if (trimmed != \"ab\") return \"fail builder snapshot\"\n\
         \x20   return \"OK\"\n\
         }\n";
    expect_box_ok_with_stdlib(source, "BuilderText");
    expect_native_box(source, "BuilderText", "OK");
}

/// `ignoreCase = true` declines: it asks about Unicode case folding, which the runtime cannot
/// answer, and answering it as though the flag were absent would be a wrong answer rather than a
/// missing one.
#[test]
fn ignoring_case_declines() {
    expect_native_decline(
        "fun box(): String = if (\"Hello\".startsWith(\"HELLO\", ignoreCase = true)) \"OK\"\n\
         \x20   else \"fail\"\n",
        "IgnoreCaseStartsWith",
        "startsWith",
    );
}
