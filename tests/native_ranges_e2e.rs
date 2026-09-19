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

/// The two UNSIGNED ranges, at the bounds where reading them signed would show.
///
/// A `UIntRange`'s bounds are stored zero-extended, so a signed 64-bit comparison of them happens
/// to answer correctly — but a `ULongRange`'s occupy all 64 bits, where `18446744073709551615uL`
/// reads as `-1` and every comparison would be wrong. Both ends of both types are here so neither
/// rests on that coincidence: the top of each range is the value a signed read calls `-1`, and an
/// empty range is the one whose `first` exceeds its `last` UNSIGNED.
///
/// Every expectation is kotlinc's, taken by compiling and running this same `box()` under it.
#[test]
fn an_unsigned_range_is_built_read_and_walked_unsigned() {
    expect_native_box(
        "fun box(): String {\n\
         \x20   val r = 1u..5u\n\
         \x20   if (r.toString() != \"1..5\") return \"fail toString: $r\"\n\
         \x20   if (!r.contains(3u)) return \"fail contains\"\n\
         \x20   if (r.contains(6u)) return \"fail contains high\"\n\
         \x20   if (r.isEmpty()) return \"fail isEmpty\"\n\
         \x20   if (r.first != 1u || r.last != 5u) return \"fail bounds: ${r.first}..${r.last}\"\n\
         \x20   var s = \"\"\n\
         \x20   for (u in r) s += \"$u,\"\n\
         \x20   if (s != \"1,2,3,4,5,\") return \"fail walk: $s\"\n\
         \x20   val top = 4294967290u..4294967295u\n\
         \x20   if (top.toString() != \"4294967290..4294967295\") return \"fail top: $top\"\n\
         \x20   if (!top.contains(4294967295u)) return \"fail top contains\"\n\
         \x20   if (top.last != 4294967295u) return \"fail top last: ${top.last}\"\n\
         \x20   var n = 0\n\
         \x20   for (u in top) n++\n\
         \x20   if (n != 6) return \"fail top walk: $n\"\n\
         \x20   val half = 1u..<4u\n\
         \x20   if (half.toString() != \"1..3\") return \"fail until: $half\"\n\
         \x20   val empty = 5u..1u\n\
         \x20   if (!empty.isEmpty()) return \"fail empty\"\n\
         \x20   if (empty.toString() != \"5..1\") return \"fail empty toString: $empty\"\n\
         \x20   val big = 18446744073709551610uL..18446744073709551615uL\n\
         \x20   if (big.toString() != \"18446744073709551610..18446744073709551615\") return \"fail ulong: $big\"\n\
         \x20   if (!big.contains(18446744073709551615uL)) return \"fail ulong contains\"\n\
         \x20   var m = 0\n\
         \x20   for (u in big) m++\n\
         \x20   if (m != 6) return \"fail ulong walk: $m\"\n\
         \x20   return \"OK\"\n\
         }\n",
        "UnsignedRanges",
        "OK",
    );
}

/// A STEPPED unsigned range, at the step and bounds where the signed ring arithmetic breaks.
///
/// The last element of a progression is the bound pulled back onto the step, and computing it
/// works modulo the step so the widest ranges never form a distance that does not fit. That
/// modulo has to be taken on the UNSIGNED ring: above 2^63 a `ULong` bound reads as a negative
/// `kt_long`, and `%` then answers the wrong half — which does not produce a wrong element but a
/// walk that never reaches its end and hangs. `0uL..ULong.MAX_VALUE step Long.MAX_VALUE` is that
/// case, and it terminates after exactly three.
///
/// Every expectation is kotlinc's, taken by compiling and running this same `box()` under it.
#[test]
fn a_stepped_unsigned_range_walks_on_the_unsigned_ring() {
    expect_native_box(
        "fun box(): String {\n\
         \x20   var n = 0\n\
         \x20   var s = \"\"\n\
         \x20   for (i in 0u..UInt.MAX_VALUE step Int.MAX_VALUE) { s += \"$i,\"; n++; if (n > 9) break }\n\
         \x20   if (s != \"0,2147483647,4294967294,\") return \"uint: $s\"\n\
         \x20   var m = 0\n\
         \x20   var t = \"\"\n\
         \x20   for (i in 0uL..ULong.MAX_VALUE step Long.MAX_VALUE) { t += \"$i,\"; m++; if (m > 9) break }\n\
         \x20   if (t != \"0,9223372036854775807,18446744073709551614,\") return \"ulong: $t\"\n\
         \x20   var d = \"\"\n\
         \x20   for (i in (1uL..9uL step 2L).reversed()) d += \"$i,\"\n\
         \x20   if (d != \"9,7,5,3,1,\") return \"rev: $d\"\n\
         \x20   return \"OK\"\n\
         }\n",
        "SteppedUnsignedRanges",
        "OK",
    );
}

/// A counted loop over an UNSIGNED range: `for (i in 1u..5u)`.
///
/// Common lowering turns a signed `for` over a range into a plain `while` before this backend sees
/// it, but an unsigned one arrives as a checked range loop instead, because the comparison and the
/// step both have to be read on the unsigned ring. The bound cases are what make that visible: a
/// walk up to `UInt.MAX_VALUE` must stop THERE rather than wrap past it, and a descending pair of
/// bounds must run zero times rather than every value on the ring.
///
/// Every expectation is kotlinc's, taken by compiling and running this same `box()` under it.
#[test]
fn a_counted_loop_over_an_unsigned_range_walks_on_the_unsigned_ring() {
    expect_native_box(
        "fun box(): String {\n\
         \x20   var sum = 0u\n\
         \x20   for (i in 1u..5u) sum += i\n\
         \x20   if (sum != 15u) return \"fail sum: $sum\"\n\
         \x20   var half = \"\"\n\
         \x20   for (i in 1u..<4u) half += \"$i,\"\n\
         \x20   if (half != \"1,2,3,\") return \"fail half: $half\"\n\
         \x20   var top = 0\n\
         \x20   for (i in (UInt.MAX_VALUE - 2u)..UInt.MAX_VALUE) top++\n\
         \x20   if (top != 3) return \"fail top: $top\"\n\
         \x20   var big = 0\n\
         \x20   for (i in (ULong.MAX_VALUE - 1uL)..ULong.MAX_VALUE) big++\n\
         \x20   if (big != 2) return \"fail big: $big\"\n\
         \x20   var none = 0\n\
         \x20   for (i in 5u..1u) none++\n\
         \x20   if (none != 0) return \"fail none: $none\"\n\
         \x20   var zero = 0\n\
         \x20   for (i in 0u..<0u) zero++\n\
         \x20   if (zero != 0) return \"fail zero: $zero\"\n\
         \x20   var lo = 0\n\
         \x20   for (i in 0uL..2uL) lo++\n\
         \x20   if (lo != 3) return \"fail lo: $lo\"\n\
         \x20   return \"OK\"\n\
         }\n",
        "UnsignedCountedLoop",
        "OK",
    );
}

/// What control flow a counted unsigned loop has to answer: `break`, `continue`, their labelled
/// forms, nesting, a `return` out of the body, and a bound whose effect must happen exactly once.
///
/// The counted loop advances its counter at the `continue` target rather than at the end of the
/// body, so a `continue` that skips the rest of the body must still step — and a `break` out of an
/// outer loop must leave both. The `return` case is the one that leaves the step unreachable.
///
/// Every expectation is kotlinc's, taken by compiling and running this same `box()` under it.
#[test]
fn a_counted_unsigned_loop_answers_break_continue_and_labels() {
    expect_native_box(
        "fun returning(): UInt {\n\
         \x20   for (i in 1u..10u) return i\n\
         \x20   return 99u\n\
         }\n\
         fun box(): String {\n\
         \x20   var s = \"\"\n\
         \x20   for (i in 1u..6u) {\n\
         \x20       if (i == 3u) continue\n\
         \x20       if (i == 5u) break\n\
         \x20       s += \"$i,\"\n\
         \x20   }\n\
         \x20   if (s != \"1,2,4,\") return \"fail plain: $s\"\n\
         \x20   var t = \"\"\n\
         \x20   outer@ for (i in 1u..3u) {\n\
         \x20       for (j in 1u..3u) {\n\
         \x20           if (j == 2u) continue@outer\n\
         \x20           if (i == 3u) break@outer\n\
         \x20           t += \"$i$j,\"\n\
         \x20       }\n\
         \x20   }\n\
         \x20   if (t != \"11,21,\") return \"fail labelled: $t\"\n\
         \x20   var n = \"\"\n\
         \x20   for (i in 1uL..3uL) for (j in 1u..2u) n += \"$i$j,\"\n\
         \x20   if (n != \"11,12,21,22,31,32,\") return \"fail nested: $n\"\n\
         \x20   if (returning() != 1u) return \"fail returning: ${returning()}\"\n\
         \x20   var effects = 0\n\
         \x20   fun end(): UInt { effects++; return 0u }\n\
         \x20   for (i in 1u..end()) { }\n\
         \x20   if (effects != 1) return \"fail effects: $effects\"\n\
         \x20   return \"OK\"\n\
         }\n",
        "UnsignedCountedLoopControl",
        "OK",
    );
}

/// The DESCENDING counted loop, `for (i in 5u downTo 1u)`.
///
/// It arrives as the same checked range loop, with the ends written the other way round, and it
/// has the mirror of the ascending trap: the walk must stop AT `0u` rather than step below it and
/// wrap round to the maximum.
///
/// Every expectation is kotlinc's, taken by compiling and running this same `box()` under it.
#[test]
fn a_descending_counted_unsigned_loop_stops_at_zero() {
    expect_native_box(
        "fun box(): String {\n\
         \x20   var d = \"\"\n\
         \x20   for (i in 5u downTo 1u) d += \"$i,\"\n\
         \x20   if (d != \"5,4,3,2,1,\") return \"fail down: $d\"\n\
         \x20   var z = 0\n\
         \x20   for (i in 2u downTo 0u) z++\n\
         \x20   if (z != 3) return \"fail zero-floor: $z\"\n\
         \x20   var none = 0\n\
         \x20   for (i in 1u downTo 5u) none++\n\
         \x20   if (none != 0) return \"fail empty down: $none\"\n\
         \x20   var l = 0\n\
         \x20   for (i in 1uL downTo 0uL) l++\n\
         \x20   if (l != 2) return \"fail ulong down: $l\"\n\
         \x20   return \"OK\"\n\
         }\n",
        "DescendingUnsignedCountedLoop",
        "OK",
    );
}

/// `downTo` and `until` on an UNSIGNED receiver, as VALUES.
///
/// Both are extensions of the unsigned ranges facade rather than members of the range they answer,
/// so the type each RETURNS is what says which runtime range to build — and `downTo` answers a
/// progression, which this runtime builds as the range it already had with a negative step beside
/// it. The bound cases are the unsigned ring again: a walk that begins at `UInt.MAX_VALUE` reads
/// its first bound above the signed maximum, and one that ends at `0u` must stop there rather than
/// step below it and wrap.
///
/// Every expectation is kotlinc's, taken by compiling and running this same `box()` under it.
#[test]
fn unsigned_down_to_and_until_answer_values() {
    expect_native_box(
        "fun walk(p: UIntProgression): String {\n\
         \x20   var s = \"\"\n\
         \x20   for (i in p) s += \"$i,\"\n\
         \x20   return s\n\
         }\n\
         fun box(): String {\n\
         \x20   val d = 5u downTo 1u\n\
         \x20   if (walk(d) != \"5,4,3,2,1,\") return \"fail down walk: ${walk(d)}\"\n\
         \x20   if (d.first != 5u) return \"fail down first: ${d.first}\"\n\
         \x20   if (d.last != 1u) return \"fail down last: ${d.last}\"\n\
         \x20   val u = 1u until 4u\n\
         \x20   if (u.toString() != \"1..3\") return \"fail until toString: $u\"\n\
         \x20   if (!u.contains(3u)) return \"fail until contains\"\n\
         \x20   if (u.last != 3u) return \"fail until last: ${u.last}\"\n\
         \x20   val top = UInt.MAX_VALUE downTo (UInt.MAX_VALUE - 2u)\n\
         \x20   if (walk(top).length != 33) return \"fail top: ${walk(top)}\"\n\
         \x20   if (top.first != UInt.MAX_VALUE) return \"fail top first: ${top.first}\"\n\
         \x20   var m = 0\n\
         \x20   for (i in 18446744073709551615uL downTo 18446744073709551613uL) m++\n\
         \x20   if (m != 3) return \"fail big: $m\"\n\
         \x20   val lu = 0uL until 3uL\n\
         \x20   if (lu.toString() != \"0..2\") return \"fail ulong until: $lu\"\n\
         \x20   if ((1u downTo 5u).isEmpty() != true) return \"fail empty\"\n\
         \x20   if (walk(9u downTo 1u step 3) != \"9,6,3,\") return \"fail stepped\"\n\
         \x20   return \"OK\"\n\
         }\n",
        "UnsignedDownToAndUntil",
        "OK",
    );
}
