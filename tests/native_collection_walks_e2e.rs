//! The members Kotlin declares over `Iterable` that the runtime answers by WALKING the receiver.
//!
//! `forEach` and `map` were already here; these are the rest of the ones a box program reaches for
//! — the predicates, the accumulations and the snapshots. Each takes the function value the program
//! wrote and hands it to the runtime as a reference, whose `invoke` the walk calls per element.
//!
//! Which iterable the receiver is stays the runtime's question, as it is for iteration itself: a
//! list, a range and an array all reach one entry point and the descriptor decides.
//!
//! TEXT does not. A string wears the iterable role so that `for (c in s)` walks it, but
//! `s.contains(t)`, `s.reversed()` and `s.first()` are questions about TEXT — one text inside
//! another, a text reversed, its first unit — and the string entry points answer those. Handing a
//! string to a collection's answers would compare a `Char` against a whole string.
//!
//! Every expectation is kotlinc's, taken by running the same program under it.

use super::common::{expect_box_ok_with_stdlib, expect_native_box};

/// The three predicate questions, over a list and over a range, and the forms with no predicate.
#[test]
fn a_walk_answers_whether_any_all_or_none_of_it_matches() {
    let source = "fun box(): String {\n\
         \x20   val xs = listOf(1, 2, 3)\n\
         \x20   if (!xs.any { it == 2 }) return \"fail any\"\n\
         \x20   if (xs.any { it == 9 }) return \"fail any miss\"\n\
         \x20   if (!xs.all { it > 0 }) return \"fail all\"\n\
         \x20   if (xs.all { it > 2 }) return \"fail all miss\"\n\
         \x20   if (!xs.none { it > 5 }) return \"fail none\"\n\
         \x20   if (xs.none { it > 2 }) return \"fail none miss\"\n\
         \x20   if (!xs.any() || xs.none()) return \"fail bare\"\n\
         \x20   if (!emptyList<Int>().none() || emptyList<Int>().any()) return \"fail bare empty\"\n\
         \x20   // A range walks the same way, and so does an array.\n\
         \x20   if (!(1..3).any { it == 3 }) return \"fail range\"\n\
         \x20   if (!intArrayOf(1, 2).all { it < 3 }) return \"fail array\"\n\
         \x20   return \"OK\"\n\
         }\n";
    expect_box_ok_with_stdlib(source, "WalkPredicates");
    expect_native_box(source, "WalkPredicates", "OK");
}

/// `any` and `all` stop at the element that settles the question, which a side effect can see.
#[test]
fn a_predicate_walk_stops_at_the_element_that_settles_it() {
    let source = "fun box(): String {\n\
         \x20   var asked = 0\n\
         \x20   val xs = listOf(1, 2, 3, 4)\n\
         \x20   xs.any { asked++; it == 2 }\n\
         \x20   if (asked != 2) return \"fail any asked \" + asked\n\
         \x20   asked = 0\n\
         \x20   xs.all { asked++; it < 3 }\n\
         \x20   if (asked != 3) return \"fail all asked \" + asked\n\
         \x20   asked = 0\n\
         \x20   xs.filter { asked++; it > 2 }\n\
         \x20   if (asked != 4) return \"fail filter asked \" + asked\n\
         \x20   return \"OK\"\n\
         }\n";
    expect_box_ok_with_stdlib(source, "WalkShortCircuit");
    expect_native_box(source, "WalkShortCircuit", "OK");
}

/// Counting, filtering and the snapshots.
#[test]
fn a_walk_counts_filters_and_snapshots_its_elements() {
    let source = "fun box(): String {\n\
         \x20   val xs = listOf(1, 2, 3)\n\
         \x20   if (xs.count() != 3) return \"fail count\"\n\
         \x20   if (xs.count { it > 1 } != 2) return \"fail count matching\"\n\
         \x20   val kept = xs.filter { it > 1 }\n\
         \x20   if (kept.size != 2 || kept[0] != 2 || kept[1] != 3) return \"fail filter\"\n\
         \x20   val dropped = xs.filterNot { it > 1 }\n\
         \x20   if (dropped.size != 1 || dropped[0] != 1) return \"fail filterNot\"\n\
         \x20   if (xs.filter { it > 9 }.size != 0) return \"fail filter none\"\n\
         \x20   if (xs.filter { true }.size != 3) return \"fail filter all\"\n\
         \x20   val reversed = xs.reversed()\n\
         \x20   if (reversed[0] != 3 || reversed[2] != 1) return \"fail reversed\"\n\
         \x20   if ((1..3).toList()[1] != 2) return \"fail range toList\"\n\
         \x20   // `IntRange.reversed()` is the RANGES facade's, answering a progression rather\n\
         \x20   // than a list — a different member under the same name, and not one of these.\n\
         \x20   var backwards = 0\n\
         \x20   for (x in (1..3).reversed()) backwards = backwards * 10 + x\n\
         \x20   if (backwards != 321) return \"fail range reversed\"\n\
         \x20   // A lazy walk has no size to ask for, and the snapshot must not need one.\n\
         \x20   if (xs.withIndex().count() != 3) return \"fail lazy count\"\n\
         \x20   if (xs.withIndex().toList().size != 3) return \"fail lazy toList\"\n\
         \x20   return \"OK\"\n\
         }\n";
    expect_box_ok_with_stdlib(source, "WalkSnapshots");
    expect_native_box(source, "WalkSnapshots", "OK");
}

/// `first`/`last` with a predicate, and the failure Kotlin raises when nothing matches.
#[test]
fn a_walk_finds_the_first_and_last_match_or_raises() {
    let source = "fun box(): String {\n\
         \x20   val xs = listOf(1, 2, 3, 4)\n\
         \x20   if (xs.first { it > 1 } != 2) return \"fail first\"\n\
         \x20   if (xs.last { it < 4 } != 3) return \"fail last\"\n\
         \x20   if (xs.firstOrNull { it > 9 } != null) return \"fail firstOrNull\"\n\
         \x20   if (xs.firstOrNull { it > 2 } != 3) return \"fail firstOrNull hit\"\n\
         \x20   var raised = 0\n\
         \x20   try {\n\
         \x20       xs.first { it > 9 }\n\
         \x20   } catch (e: NoSuchElementException) {\n\
         \x20       raised++\n\
         \x20   }\n\
         \x20   try {\n\
         \x20       xs.last { it > 9 }\n\
         \x20   } catch (e: NoSuchElementException) {\n\
         \x20       raised++\n\
         \x20   }\n\
         \x20   return if (raised == 2) \"OK\" else \"fail raised \" + raised\n\
         }\n";
    expect_box_ok_with_stdlib(source, "WalkFinds");
    expect_native_box(source, "WalkFinds", "OK");
}

/// Accumulations: `fold`, `sumOf` at each width the runtime answers, and `forEachIndexed`.
#[test]
fn a_walk_accumulates_through_fold_sum_and_the_indexed_walk() {
    let source = "fun box(): String {\n\
         \x20   val xs = listOf(1, 2, 3)\n\
         \x20   if (xs.fold(0) { acc, e -> acc + e } != 6) return \"fail fold\"\n\
         \x20   if (xs.fold(\"\") { acc, e -> acc + e } != \"123\") return \"fail fold text\"\n\
         \x20   if (emptyList<Int>().fold(7) { acc, e -> acc + e } != 7) return \"fail fold empty\"\n\
         \x20   if (xs.sumOf { it * 2 } != 12) return \"fail sumOf\"\n\
         \x20   if (xs.sumOf { it.toLong() * 3L } != 18L) return \"fail sumOf long\"\n\
         \x20   if (xs.sumOf { it.toDouble() / 2.0 } != 3.0) return \"fail sumOf double\"\n\
         \x20   var seen = \"\"\n\
         \x20   xs.forEachIndexed { index, value -> seen += \"\" + index + value }\n\
         \x20   if (seen != \"011223\") return \"fail forEachIndexed \" + seen\n\
         \x20   return \"OK\"\n\
         }\n";
    expect_box_ok_with_stdlib(source, "WalkAccumulations");
    expect_native_box(source, "WalkAccumulations", "OK");
}

/// Membership over each iterable the runtime has.
#[test]
fn a_walk_answers_whether_it_holds_a_value_and_where() {
    let source = "fun box(): String {\n\
         \x20   val xs: Iterable<Int> = listOf(1, 2, 3)\n\
         \x20   if (2 !in xs) return \"fail in\"\n\
         \x20   if (9 in xs) return \"fail in miss\"\n\
         \x20   if (xs.indexOf(3) != 2) return \"fail indexOf\"\n\
         \x20   if (xs.indexOf(9) != -1) return \"fail indexOf miss\"\n\
         \x20   if (2 !in intArrayOf(1, 2, 3)) return \"fail array\"\n\
         \x20   val texts: Iterable<String> = listOf(\"a\", \"b\")\n\
         \x20   // Elements are compared with `equals`, not by identity.\n\
         \x20   if ((\"a\" + \"\") !in texts) return \"fail equals\"\n\
         \x20   return \"OK\"\n\
         }\n";
    expect_box_ok_with_stdlib(source, "WalkMembership");
    expect_native_box(source, "WalkMembership", "OK");
}

/// `plus` answers a NEW list and leaves the receiver as it was.
#[test]
fn adding_to_a_list_answers_a_new_one() {
    let source = "fun box(): String {\n\
         \x20   val xs = listOf(1, 2)\n\
         \x20   val one = xs + 3\n\
         \x20   if (one.size != 3 || one[2] != 3) return \"fail plus\"\n\
         \x20   val both = xs + listOf(3, 4)\n\
         \x20   if (both.size != 4 || both[3] != 4) return \"fail plus all\"\n\
         \x20   if (xs.size != 2) return \"fail receiver changed\"\n\
         \x20   if ((emptyList<Int>() + 1).size != 1) return \"fail plus onto empty\"\n\
         \x20   return \"OK\"\n\
         }\n";
    expect_box_ok_with_stdlib(source, "ListPlus");
    expect_native_box(source, "ListPlus", "OK");
}

/// The array members: a reversed ARRAY keeps its element type, and the `content…` three.
#[test]
fn an_array_reverses_into_an_array_and_answers_the_content_questions() {
    let source = "fun box(): String {\n\
         \x20   val ints = intArrayOf(1, 2, 3)\n\
         \x20   val backwards = ints.reversedArray()\n\
         \x20   if (backwards[0] != 3 || backwards[2] != 1) return \"fail reversedArray\"\n\
         \x20   // A snapshot: writing through the source leaves it as it was.\n\
         \x20   ints[0] = 9\n\
         \x20   if (backwards[2] != 1) return \"fail snapshot\"\n\
         \x20   val texts = arrayOf(\"a\", \"b\")\n\
         \x20   if (texts.reversedArray()[0] != \"b\") return \"fail object reversedArray\"\n\
         \x20   if (!intArrayOf(1, 2).contentEquals(intArrayOf(1, 2))) return \"fail contentEquals\"\n\
         \x20   if (intArrayOf(1, 2).contentEquals(intArrayOf(2, 1))) return \"fail order\"\n\
         \x20   if (intArrayOf(1).contentEquals(intArrayOf(1, 2))) return \"fail length\"\n\
         \x20   if (arrayOf(\"a\").contentEquals(arrayOf(\"a\")) != true) return \"fail object equals\"\n\
         \x20   if (intArrayOf(1, 2).contentHashCode() != intArrayOf(1, 2).contentHashCode())\n\
         \x20       return \"fail contentHashCode\"\n\
         \x20   if (intArrayOf(1, 2).contentToString() != \"[1, 2]\") return \"fail contentToString\"\n\
         \x20   if (intArrayOf().contentToString() != \"[]\") return \"fail empty contentToString\"\n\
         \x20   return \"OK\"\n\
         }\n";
    expect_box_ok_with_stdlib(source, "ArrayContent");
    expect_native_box(source, "ArrayContent", "OK");
}

/// TEXT wears the iterable role and is still answered as text.
#[test]
fn a_string_is_not_a_collection_of_its_characters_here() {
    let source = "fun box(): String {\n\
         \x20   // Each of these names a member a collection also declares, and each must take the\n\
         \x20   // TEXT answer: `contains` asks about one text inside another, `reversed` answers a\n\
         \x20   // text, `first` a unit of one.\n\
         \x20   if (!\"abcd\".contains(\"bc\")) return \"fail contains\"\n\
         \x20   if (\"abcd\".reversed() != \"dcba\") return \"fail reversed\"\n\
         \x20   if (\"abcd\".first() != 'a') return \"fail first\"\n\
         \x20   // Iteration still walks the characters.\n\
         \x20   var seen = \"\"\n\
         \x20   for (c in \"OK\") seen += c\n\
         \x20   return seen\n\
         }\n";
    expect_box_ok_with_stdlib(source, "TextIsNotACollection");
    expect_native_box(source, "TextIsNotACollection", "OK");
}
