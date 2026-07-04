//! Character- and string-literal escape sequences the box corpus never spells out — `\r`, `\b`, `\\`,
//! `\"`, `\'`, `\$`, and `\uXXXX`. The parser's `unquote_char`/`unescape_chunk` decode every escape
//! whenever a literal is parsed, but the corpus only exercises the common `\n`/`\t`, leaving the rest
//! of the match arms untouched. Each escape is checked against its `\uXXXX` equivalent so a wrong
//! decode fails loudly.

mod common;

fn run_ok(stem: &str, body: &str) {
    common::expect_box_ok_with_stdlib(body, stem);
}

#[test]
fn char_literal_escapes() {
    run_ok(
        "CharEsc",
        "fun box(): String {\n\
         if ('\\r' != '\\u000D') return \"cr\"\n\
         if ('\\b' != '\\u0008') return \"bs\"\n\
         if ('\\t' != '\\u0009') return \"tab\"\n\
         if ('\\n' != '\\u000A') return \"nl\"\n\
         if ('\\\\' != '\\u005C') return \"bsl\"\n\
         if ('\\'' != '\\u0027') return \"sq\"\n\
         if ('\\u0041' != 'A') return \"cu\"\n\
         return \"OK\"\n\
         }\n",
    );
}

#[test]
fn string_literal_escapes() {
    run_ok(
        "StrEsc",
        "fun box(): String {\n\
         val s = \"\\t\\b\\n\\r\\\\\\\"\\$\\u0041\"\n\
         val exp = \"\\u0009\\u0008\\u000A\\u000D\\u005C\\u0022\\u0024\\u0041\"\n\
         if (s != exp) return \"str=$s\"\n\
         return \"OK\"\n\
         }\n",
    );
}

#[test]
fn range_operator_kinds() {
    // `..`, `..<`, `until`, and `downTo` range forms — the parser has a distinct arm per kind, and the
    // corpus doesn't spell out every one in this position. Sum each range to prove it built correctly.
    run_ok(
        "RangeKinds",
        "fun box(): String {\n\
         var a = 0; for (i in 1..3) a += i\n\
         if (a != 6) return \"through=$a\"\n\
         var b = 0; for (i in 1..<4) b += i\n\
         if (b != 6) return \"untilOp=$b\"\n\
         var c = 0; for (i in 1 until 4) c += i\n\
         if (c != 6) return \"untilWord=$c\"\n\
         var d = 0; for (i in 3 downTo 1) d += i\n\
         if (d != 6) return \"downTo=$d\"\n\
         return \"OK\"\n\
         }\n",
    );
}
