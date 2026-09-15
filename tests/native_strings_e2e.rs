//! Strings through krusty's own runtime: text stored as UTF-8, indexed as Kotlin indexes it.
//!
//! Kotlin's `String` is a sequence of UTF-16 code units and krusty's native runtime stores UTF-8,
//! so every question about a position is a question about a walk. These programs pin that the walk
//! answers what the JVM answers, including where the two encodings disagree most: a character
//! outside the BMP is one UTF-8 sequence and TWO Kotlin indices.

use super::common::{expect_box_run_with_stdlib, expect_native_box};

#[test]
fn a_string_is_indexed_by_utf16_code_unit() {
    // The text is stored as UTF-8 and Kotlin indexes by UTF-16 unit, so a character outside the
    // BMP occupies TWO indices and reads back as the surrogate pair Kotlin stores.
    expect_native_box(
        "fun box(): String {\n\
         \x20   val text = \"a\\u00E9\\u4E2D\\uD83D\\uDE00z\"\n\
         \x20   if (text.length != 6) return \"fail length: \" + text.length\n\
         \x20   if (text[0] != 'a') return \"fail 0\"\n\
         \x20   if (text[1] != '\\u00E9') return \"fail 1\"\n\
         \x20   if (text[2] != '\\u4E2D') return \"fail 2\"\n\
         \x20   if (text[3] != '\\uD83D') return \"fail 3\"\n\
         \x20   if (text[4] != '\\uDE00') return \"fail 4\"\n\
         \x20   if (text[5] != 'z') return \"fail 5\"\n\
         \x20   return \"OK\"\n\
         }\n",
        "StringIndexing",
        "OK",
    );
}

#[test]
fn a_substring_is_taken_by_utf16_unit() {
    expect_native_box(
        "fun box(): String {\n\
         \x20   val s = \"hello world\"\n\
         \x20   if (s.substring(0, 5) != \"hello\") return \"fail head: ${s.substring(0, 5)}\"\n\
         \x20   if (s.substring(6, 11) != \"world\") return \"fail tail: ${s.substring(6, 11)}\"\n\
         \x20   if (s.substring(6) != \"world\") return \"fail open: ${s.substring(6)}\"\n\
         \x20   if (s.substring(5, 5) != \"\") return \"fail empty: ${s.substring(5, 5)}\"\n\
         \x20   if (s.substring(0, 11) != s) return \"fail whole\"\n\
         \x20   return \"OK\"\n\
         }\n",
        "SubstringByUnit",
        "OK",
    );
}

#[test]
fn a_substring_of_text_outside_ascii_counts_the_units_kotlin_counts() {
    // `é` is two BYTES and one unit; `𝄞` is four bytes and TWO units. A slice that counted bytes
    // would answer something else for every index past the first of them.
    expect_native_box(
        "fun box(): String {\n\
         \x20   val s = \"a\\u00e9b\"\n\
         \x20   if (s.length != 3) return \"fail length: ${s.length}\"\n\
         \x20   if (s.substring(1, 2) != \"\\u00e9\") return \"fail middle\"\n\
         \x20   if (s.substring(2) != \"b\") return \"fail tail: ${s.substring(2)}\"\n\
         \x20   val wide = \"a\\uD834\\uDD1Eb\"\n\
         \x20   if (wide.length != 4) return \"fail wide length: ${wide.length}\"\n\
         \x20   if (wide.substring(3) != \"b\") return \"fail past the pair: ${wide.substring(3)}\"\n\
         \x20   if (wide.substring(1, 3) != \"\\uD834\\uDD1E\") return \"fail the pair itself\"\n\
         \x20   return \"OK\"\n\
         }\n",
        "SubstringOutsideAscii",
        "OK",
    );
}

#[test]
fn a_subsequence_is_the_same_slice_and_answers_its_length() {
    expect_native_box(
        "fun String.drop2(): CharSequence? = if (length >= 2) subSequence(2, length) else null\n\
         fun box(): String {\n\
         \x20   val dropped = \"abcd\".drop2()\n\
         \x20   if (dropped?.length != 2) return \"fail length: ${dropped?.length}\"\n\
         \x20   if (dropped.toString() != \"cd\") return \"fail text: $dropped\"\n\
         \x20   return if (\"a\".drop2() == null) \"OK\" else \"fail short\"\n\
         }\n",
        "SubSequenceLength",
        "OK",
    );
}

#[test]
fn strings_order_by_their_units() {
    expect_native_box(
        "fun box(): String {\n\
         \x20   if (!(\"b\" > \"a\")) return \"fail greater\"\n\
         \x20   if (\"a\" > \"b\") return \"fail less\"\n\
         \x20   if (\"abc\".compareTo(\"abc\") != 0) return \"fail equal\"\n\
         \x20   if (\"ab\" >= \"abc\") return \"fail prefix\"\n\
         \x20   if (\"\" >= \"a\") return \"fail empty\"\n\
         \x20   return \"OK\"\n\
         }\n",
        "StringOrdering",
        "OK",
    );
}

#[test]
fn a_suffix_is_removed_only_when_the_string_ends_there() {
    expect_native_box(
        "fun box(): String {\n\
         \x20   if (\"file.txt\".removeSuffix(\".txt\") != \"file\") return \"fail 1\"\n\
         \x20   if (\"file.txt\".removeSuffix(\".md\") != \"file.txt\") return \"fail 2\"\n\
         \x20   if (\"ab\".removeSuffix(\"abcd\") != \"ab\") return \"fail longer than the string\"\n\
         \x20   if (\"ab\".removeSuffix(\"\") != \"ab\") return \"fail empty suffix\"\n\
         \x20   if (\"aa\".removeSuffix(\"aa\") != \"\") return \"fail whole\"\n\
         \x20   return \"OK\"\n\
         }\n",
        "RemoveSuffix",
        "OK",
    );
}

#[test]
fn the_comparison_magnitude_agrees_with_the_jvm_backend() {
    // `compareTo` answers more than a sign in Kotlin, and a program may print it: the difference of
    // the first units that differ, or of the lengths when one is a prefix. The other backend is the
    // oracle for that, not my reading of it.
    assert_eq!(
        expect_box_run_with_stdlib(
            "fun box(): String {\n\
             \x20   val head = \"${\"abc\".compareTo(\"abd\")}/${\"abd\".compareTo(\"abc\")}\"\n\
             \x20   val tail = \"${\"ab\".compareTo(\"abcd\")}/${\"abcd\".compareTo(\"ab\")}\"\n\
             \x20   return \"$head/$tail/${\"a\".compareTo(\"a\")}\"\n\
             }\n",
            "StringCompareMagnitude",
        ),
        "-1/1/-2/2/0"
    );
}
