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

#[test]
fn an_arrays_indices_are_a_range_of_its_positions() {
    expect_native_box(
        "fun box(): String {\n\
         \x20   val values = Array<Int>(5) { it }\n\
         \x20   var sum = 0\n\
         \x20   for (index in values.indices) sum += values[index]\n\
         \x20   if (sum != 10) return \"fail sum: $sum\"\n\
         \x20   if (values.indices != 0..4) return \"fail range: \" + values.indices\n\
         \x20   if (5 in values.indices) return \"fail: 5 is an index\"\n\
         \x20   return \"OK\"\n\
         }\n",
        "ArrayIndices",
        "OK",
    );
}

#[test]
fn an_empty_indexable_has_empty_indices() {
    // `0..size - 1` of an empty receiver is `0..-1`, which is the empty range — not a wrap.
    expect_native_box(
        "fun box(): String {\n\
         \x20   val values = IntArray(0)\n\
         \x20   if (!values.indices.isEmpty()) return \"fail: array indices are not empty\"\n\
         \x20   if (0 in values.indices) return \"fail: 0 is an index\"\n\
         \x20   if (!\"\".indices.isEmpty()) return \"fail: string indices are not empty\"\n\
         \x20   return \"OK\"\n\
         }\n",
        "EmptyIndices",
        "OK",
    );
}

#[test]
fn a_strings_indices_count_its_characters() {
    expect_native_box(
        "fun box(): String {\n\
         \x20   val text = \"abcd\"\n\
         \x20   var joined = \"\"\n\
         \x20   for (index in text.indices) joined += text[index]\n\
         \x20   if (joined != \"abcd\") return \"fail: $joined\"\n\
         \x20   return if (text.indices == 0..3) \"OK\" else \"fail range: \" + text.indices\n\
         }\n",
        "StringIndices",
        "OK",
    );
}

#[test]
fn a_range_membership_test_builds_no_range() {
    // `x in a..b` reaches the generator as its BOUNDS, not as a range, so the whole of it is two
    // comparisons. Each form puts them in a different place: `..<` excludes its high end, and
    // `downTo` writes its ends the other way round, so the low one is the second.
    expect_native_box(
        "fun box(): String {\n\
         \x20   if (5 !in 1..10) return \"fail inside\"\n\
         \x20   if (1 !in 1..10) return \"fail low end\"\n\
         \x20   if (10 !in 1..10) return \"fail high end\"\n\
         \x20   if (0 in 1..10) return \"fail below\"\n\
         \x20   if (11 in 1..10) return \"fail above\"\n\
         \x20   if (10 !in 1..<11) return \"fail open inside\"\n\
         \x20   if (11 in 1..<11) return \"fail open end\"\n\
         \x20   if (5 !in 10 downTo 1) return \"fail downTo inside\"\n\
         \x20   if (0 in 10 downTo 1) return \"fail downTo below\"\n\
         \x20   if (11 in 10 downTo 1) return \"fail downTo above\"\n\
         \x20   if (3 in 10..1) return \"fail empty\"\n\
         \x20   return \"OK\"\n\
         }\n",
        "RangeMembership",
        "OK",
    );
}

#[test]
fn a_range_membership_test_reads_its_counter_the_right_way() {
    // `Char` and the unsigned integers read their top bit as a value. A `Char`'s carrier is narrow
    // enough that a signed comparison would call its upper half negative, and `ULong.MAX_VALUE` is
    // `-1` read as a sign.
    expect_native_box(
        "fun box(): String {\n\
         \x20   if ('c' !in 'a'..'z') return \"fail char\"\n\
         \x20   if ('A' in 'a'..'z') return \"fail char below\"\n\
         \x20   if ('\\uFFFE' !in '\\uFFF0'..'\\uFFFF') return \"fail high char\"\n\
         \x20   val big: ULong = 18446744073709551615uL\n\
         \x20   if (big !in 1uL..big) return \"fail ulong\"\n\
         \x20   if (1uL in 2uL..big) return \"fail ulong below\"\n\
         \x20   val mid: UInt = 3000000000u\n\
         \x20   if (mid !in 1u..4000000000u) return \"fail uint\"\n\
         \x20   return \"OK\"\n\
         }\n",
        "RangeMembershipCounters",
        "OK",
    );
}

#[test]
fn a_range_membership_test_evaluates_its_bounds_before_its_subject() {
    // `x in a..b` is `(a..b).contains(x)`, and a receiver is evaluated before an argument — so both
    // ends run before the subject does, and both run even though the first comparison settles this
    // answer on its own. Comparing without building a range must not change either fact.
    expect_native_box(
        "var trail = \"\"\n\
         fun low(): Int {\n\
         \x20   trail += \"l\"\n\
         \x20   return 10\n\
         }\n\
         fun high(): Int {\n\
         \x20   trail += \"h\"\n\
         \x20   return 20\n\
         }\n\
         fun subject(): Int {\n\
         \x20   trail += \"s\"\n\
         \x20   return 0\n\
         }\n\
         fun box(): String {\n\
         \x20   val inside = subject() in low()..high()\n\
         \x20   if (inside) return \"fail answer\"\n\
         \x20   return if (trail == \"lhs\") \"OK\" else \"fail order: $trail\"\n\
         }\n",
        "RangeMembershipEffects",
        "OK",
    );
}

#[test]
fn a_progression_steps_descends_and_reverses() {
    // A progression is a range with a step, and Kotlin's `last` is the last element actually
    // REACHED — `(1..9 step 3).last` is 7, not 9. That is observable, and it is what makes
    // `reversed()` start at 7 rather than at 9.
    expect_native_box(
        "fun walk(r: Iterable<Int>): String {\n\
         \x20   var out = \"\"\n\
         \x20   for (x in r) out += \"$x,\"\n\
         \x20   return out\n\
         }\n\
         fun box(): String {\n\
         \x20   if (walk(1..10 step 3) != \"1,4,7,10,\") return \"fail step: ${walk(1..10 step 3)}\"\n\
         \x20   if (walk(1..9 step 3) != \"1,4,7,\") return \"fail short step: ${walk(1..9 step 3)}\"\n\
         \x20   if (walk(10 downTo 1) != \"10,9,8,7,6,5,4,3,2,1,\") return \"fail downTo\"\n\
         \x20   if (walk(10 downTo 1 step 3) != \"10,7,4,1,\") return \"fail downTo step\"\n\
         \x20   if (walk((1..9 step 3).reversed()) != \"7,4,1,\") return \"fail reversed\"\n\
         \x20   if (walk((1..3).reversed()) != \"3,2,1,\") return \"fail reversed plain\"\n\
         \x20   if (walk(1 downTo 10) != \"\") return \"fail empty downTo\"\n\
         \x20   if (walk(10..1 step 2) != \"\") return \"fail empty step\"\n\
         \x20   return \"OK\"\n\
         }\n",
        "Progressions",
        "OK",
    );
}

#[test]
fn a_progression_answers_its_own_bounds() {
    // `first` and `last` are the progression's, not the bounds it was written with.
    expect_native_box(
        "fun box(): String {\n\
         \x20   val p = 1..9 step 3\n\
         \x20   if (p.first != 1) return \"fail first\"\n\
         \x20   if (p.last != 7) return \"fail last: ${p.last}\"\n\
         \x20   val d = 10 downTo 2 step 3\n\
         \x20   if (d.first != 10) return \"fail down first\"\n\
         \x20   if (d.last != 4) return \"fail down last: ${d.last}\"\n\
         \x20   return \"OK\"\n\
         }\n",
        "ProgressionBounds",
        "OK",
    );
}

#[test]
fn with_index_numbers_every_element_of_a_range() {
    // `withIndex` keeps the range as a VALUE -- it wraps the `Iterable`, so the loop walks the
    // wrapper and the range has to answer `iterator` for itself rather than be unrolled into a
    // counted loop.
    expect_native_box(
        "fun box(): String {\n\
         \x20   var seen = \"\"\n\
         \x20   for (iv in (4..7).withIndex()) seen += \"${iv.index}:${iv.value},\"\n\
         \x20   return if (seen == \"0:4,1:5,2:6,3:7,\") \"OK\" else \"fail: $seen\"\n\
         }\n",
        "RangeWithIndex",
        "OK",
    );
}

#[test]
fn a_progression_spanning_the_whole_range_reaches_every_step() {
    // The widest walk a `Long` has. Its two bounds are further apart than a `Long` can hold, so a
    // last element computed as `first + ((last - first) / step) * step` wraps and the walk stops
    // after one element; working modulo the step never forms that distance.
    expect_native_box(
        "fun box(): String {\n\
         \x20   var up = \"\"\n\
         \x20   for (v in Long.MIN_VALUE..Long.MAX_VALUE step Long.MAX_VALUE) up += \"$v,\"\n\
         \x20   if (up != \"-9223372036854775808,-1,9223372036854775806,\") return \"fail up: $up\"\n\
         \x20   var down = \"\"\n\
         \x20   for (v in Long.MAX_VALUE downTo Long.MIN_VALUE step Long.MAX_VALUE) down += \"$v,\"\n\
         \x20   if (down != \"9223372036854775807,0,-9223372036854775807,\") return \"fail down: $down\"\n\
         \x20   return \"OK\"\n\
         }\n",
        "WidestProgression",
        "OK",
    );
}
