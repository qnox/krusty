//! Ranges as VALUES through krusty's own code generator and runtime.
//!
//! A range a loop consumes never becomes an object — common lowering turns it into a counted loop
//! first — so every program here keeps the range instead: stores it, asks it a question, renders
//! it. Each must be LOWERED, not declined, which is what `expect_native_box` claims.

use super::common::expect_native_box;

#[test]
fn a_stored_int_range_answers_membership() {
    expect_native_box(
        "val range = 1..3\n\
         fun box(): String = when {\n\
         \x20   0 in range -> \"fail 1\"\n\
         \x20   1 !in range -> \"fail 2\"\n\
         \x20   3 !in range -> \"fail 3\"\n\
         \x20   4 in range -> \"fail 4\"\n\
         \x20   else -> \"OK\"\n\
         }\n",
        "StoredIntRange",
        "OK",
    );
}

#[test]
fn a_half_open_range_excludes_its_end() {
    expect_native_box(
        "fun box(): String {\n\
         \x20   val range = 1 until 3\n\
         \x20   if (3 in range) return \"fail: 3 is in it\"\n\
         \x20   if (2 !in range) return \"fail: 2 is not\"\n\
         \x20   return \"OK\"\n\
         }\n",
        "HalfOpenRange",
        "OK",
    );
}

#[test]
fn until_the_minimum_answers_the_empty_range() {
    // Kotlin's `until` answers the EMPTY range rather than wrapping `Int.MIN_VALUE - 1` round to
    // the maximum, which would make every value a member.
    expect_native_box(
        "fun box(): String {\n\
         \x20   val range = 0 until Int.MIN_VALUE\n\
         \x20   if (0 in range) return \"fail: 0 is in it\"\n\
         \x20   if (Int.MAX_VALUE in range) return \"fail: MAX_VALUE is in it\"\n\
         \x20   return if (range.isEmpty()) \"OK\" else \"fail: not empty\"\n\
         }\n",
        "UntilMinimum",
        "OK",
    );
}

#[test]
fn a_range_answers_its_bounds() {
    expect_native_box(
        "fun box(): String {\n\
         \x20   val range = 4..9\n\
         \x20   if (range.first != 4) return \"fail first: \" + range.first\n\
         \x20   if (range.last != 9) return \"fail last: \" + range.last\n\
         \x20   if (range.isEmpty()) return \"fail: empty\"\n\
         \x20   if (!(9..4).isEmpty()) return \"fail: 9..4 is not empty\"\n\
         \x20   return \"OK\"\n\
         }\n",
        "RangeBounds",
        "OK",
    );
}

#[test]
fn a_long_range_keeps_its_width() {
    expect_native_box(
        "fun box(): String {\n\
         \x20   val range = 1L..0x1_0000_0000L\n\
         \x20   if (0x1_0000_0000L !in range) return \"fail: end excluded\"\n\
         \x20   if (0x1_0000_0001L in range) return \"fail: past the end\"\n\
         \x20   if (range.last != 0x1_0000_0000L) return \"fail last: \" + range.last\n\
         \x20   return \"OK\"\n\
         }\n",
        "LongRangeValue",
        "OK",
    );
}

#[test]
fn a_char_range_compares_unsigned() {
    // A `Char` above 0x7FFF read as a signed 16-bit number would be negative, and every comparison
    // against a bound would answer backwards.
    expect_native_box(
        "fun box(): String {\n\
         \x20   val letters = 'a'..'z'\n\
         \x20   if ('q' !in letters) return \"fail: q\"\n\
         \x20   if ('A' in letters) return \"fail: A\"\n\
         \x20   val high = '\\uF000'..'\\uFFFF'\n\
         \x20   if ('\\uFF00' !in high) return \"fail: high\"\n\
         \x20   if ('a' in high) return \"fail: a is high\"\n\
         \x20   return \"OK\"\n\
         }\n",
        "CharRangeValue",
        "OK",
    );
}

#[test]
fn a_range_renders_and_compares_as_kotlin_declares() {
    expect_native_box(
        "fun box(): String {\n\
         \x20   val range = 1..3\n\
         \x20   if (\"$range\" != \"1..3\") return \"fail toString: $range\"\n\
         \x20   if (range != 1..3) return \"fail equals\"\n\
         \x20   if (range == 1..4) return \"fail unequal\"\n\
         \x20   if ((3..1) != (5..2)) return \"fail: empty ranges are equal\"\n\
         \x20   if (range.hashCode() != (1..3).hashCode()) return \"fail hashCode\"\n\
         \x20   if ((3..1).hashCode() != -1) return \"fail empty hashCode\"\n\
         \x20   return \"OK\"\n\
         }\n",
        "RangeIdentity",
        "OK",
    );
}

#[test]
fn a_char_range_renders_its_bounds_as_characters() {
    expect_native_box(
        "fun box(): String {\n\
         \x20   val letters = 'a'..'c'\n\
         \x20   return if (\"$letters\" == \"a..c\") \"OK\" else \"fail: $letters\"\n\
         }\n",
        "CharRangeRendering",
        "OK",
    );
}

#[test]
fn a_range_is_a_value_that_travels() {
    expect_native_box(
        "fun widest(one: IntRange, other: IntRange): IntRange =\n\
         \x20   if (one.last - one.first >= other.last - other.first) one else other\n\
         fun box(): String {\n\
         \x20   val answer = widest(1..3, 10..20)\n\
         \x20   return if (answer == 10..20) \"OK\" else \"fail: $answer\"\n\
         }\n",
        "RangeAsValue",
        "OK",
    );
}

#[test]
fn a_materialized_range_iterates() {
    // A range written straight into the loop never becomes an object — common lowering turns it
    // into a counted loop. One the program built first does, and the loop reads it through its
    // iterator.
    expect_native_box(
        "fun box(): String {\n\
         \x20   var sum = 0\n\
         \x20   val range = 1..4\n\
         \x20   for (value in range) sum += value\n\
         \x20   if (sum != 10) return \"fail sum: $sum\"\n\
         \x20   var count = 0\n\
         \x20   for (value in 3..1) count++\n\
         \x20   return if (count == 0) \"OK\" else \"fail: an empty range ran $count times\"\n\
         }\n",
        "RangeIteration",
        "OK",
    );
}

#[test]
fn iterating_to_the_maximum_terminates() {
    // `next + 1` past the element type's maximum wraps, so a `next <= last` test would never stop.
    expect_native_box(
        "fun box(): String {\n\
         \x20   var count = 0\n\
         \x20   val range = (Int.MAX_VALUE - 2)..Int.MAX_VALUE\n\
         \x20   for (value in range) count++\n\
         \x20   return if (count == 3) \"OK\" else \"fail: $count\"\n\
         }\n",
        "RangeIterationToMaximum",
        "OK",
    );
}

#[test]
fn a_char_range_iterates_as_characters() {
    expect_native_box(
        "fun box(): String {\n\
         \x20   var text = \"\"\n\
         \x20   val letters = 'a'..'d'\n\
         \x20   for (letter in letters) text += letter\n\
         \x20   return if (text == \"abcd\") \"OK\" else \"fail: $text\"\n\
         }\n",
        "CharRangeIteration",
        "OK",
    );
}
