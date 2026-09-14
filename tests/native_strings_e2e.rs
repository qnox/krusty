//! Strings through krusty's own runtime: text stored as UTF-8, indexed as Kotlin indexes it.
//!
//! Kotlin's `String` is a sequence of UTF-16 code units and krusty's native runtime stores UTF-8,
//! so every question about a position is a question about a walk. These programs pin that the walk
//! answers what the JVM answers, including where the two encodings disagree most: a character
//! outside the BMP is one UTF-8 sequence and TWO Kotlin indices.

use super::common::expect_native_box;

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
