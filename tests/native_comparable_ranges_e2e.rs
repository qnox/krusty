//! `"a".."c"` — a range whose bounds are ordered by `Comparable`, not by a machine comparison.
//!
//! Kotlin declares `rangeTo` on `Comparable<T>` and answers a `ComparableRange<T>`, seen through
//! `ClosedRange<T>`. It keeps the two bounds as OBJECTS and asks each one how it compares, so the
//! runtime object here does the same through the receiver's descriptor. It is no progression:
//! `Comparable` names no successor, so there is no step and no walk — a pair of bounds and the
//! question `value in it`.
//!
//! Beside it, the INTEGRAL ranges reached through the same interface: `fun f(r: ClosedRange<Int>)`
//! names `ClosedRange` where the object is an `IntRange`, and a bound read there is the erased `T`,
//! so it comes back boxed at the element's own width.
//!
//! Every expectation is kotlinc's, taken by running the same program under it.

use super::common::{expect_box_ok_with_stdlib, expect_native_box, expect_native_decline};

/// A range of strings: membership, emptiness, its bounds, and how it renders.
#[test]
fn a_range_of_strings_answers_through_compare_to() {
    let source = "fun box(): String {\n\
         \x20   val range = \"a\"..\"c\"\n\
         \x20   if (\"b\" !in range) return \"fail in\"\n\
         \x20   if (\"a\" !in range) return \"fail low bound\"\n\
         \x20   if (\"c\" !in range) return \"fail high bound\"\n\
         \x20   if (\"d\" in range) return \"fail past\"\n\
         \x20   if (\"A\" in range) return \"fail before\"\n\
         \x20   if (range.isEmpty()) return \"fail isEmpty\"\n\
         \x20   if (range.start != \"a\") return \"fail start \" + range.start\n\
         \x20   if (range.endInclusive != \"c\") return \"fail end \" + range.endInclusive\n\
         \x20   if (range.toString() != \"a..c\") return \"fail toString \" + range\n\
         \x20   val empty = \"c\"..\"a\"\n\
         \x20   if (!empty.isEmpty()) return \"fail empty\"\n\
         \x20   if (\"b\" in empty) return \"fail empty holds\"\n\
         \x20   return \"OK\"\n\
         }\n";
    expect_box_ok_with_stdlib(source, "ComparableRangeOfStrings");
    expect_native_box(source, "ComparableRangeOfStrings", "OK");
}

/// Two are equal when both are empty, whatever their bounds — `ClosedRange`'s documented contract,
/// and an empty one hashes to `-1`.
#[test]
fn two_comparable_ranges_are_equal_by_their_bounds_or_by_being_empty() {
    let source = "fun box(): String {\n\
         \x20   if ((\"a\"..\"c\") != (\"a\"..\"c\")) return \"fail equal\"\n\
         \x20   if ((\"a\"..\"c\") == (\"a\"..\"d\")) return \"fail unequal\"\n\
         \x20   if ((\"c\"..\"a\") != (\"z\"..\"b\")) return \"fail empty equal\"\n\
         \x20   if ((\"a\"..\"c\").hashCode() != (\"a\"..\"c\").hashCode()) return \"fail hash\"\n\
         \x20   if ((\"c\"..\"a\").hashCode() != -1) return \"fail empty hash\"\n\
         \x20   return \"OK\"\n\
         }\n";
    expect_box_ok_with_stdlib(source, "ComparableRangeEquality");
    expect_native_box(source, "ComparableRangeEquality", "OK");
}

/// An INTEGRAL range held under `ClosedRange<T>`: the member names the interface, the object is
/// still the range the literal built, and a bound comes back at the element's own width.
#[test]
fn an_integral_range_answers_through_the_interface() {
    let source = "fun holds(range: ClosedRange<Int>, value: Int): Boolean = value in range\n\
         fun holdsLong(range: ClosedRange<Long>, value: Long): Boolean = value in range\n\
         fun box(): String {\n\
         \x20   if (!holds(1..3, 2)) return \"fail in\"\n\
         \x20   if (holds(1..3, 4)) return \"fail past\"\n\
         \x20   if (holds(3..1, 2)) return \"fail empty\"\n\
         \x20   if (!holdsLong(1L..3L, 3L)) return \"fail long\"\n\
         \x20   val range: ClosedRange<Int> = 1..3\n\
         \x20   if (range.start != 1) return \"fail start \" + range.start\n\
         \x20   if (range.endInclusive != 3) return \"fail end \" + range.endInclusive\n\
         \x20   if (range.isEmpty()) return \"fail isEmpty\"\n\
         \x20   val chars: ClosedRange<Char> = 'a'..'c'\n\
         \x20   if (chars.start != 'a') return \"fail char start \" + chars.start\n\
         \x20   return \"OK\"\n\
         }\n";
    expect_box_ok_with_stdlib(source, "ClosedRangeThroughInterface");
    expect_native_box(source, "ClosedRangeThroughInterface", "OK");
}

/// A file that declares its OWN `ClosedRange` declines, as every runtime-answered member does: no
/// static type tells the program's object from the one the runtime built.
#[test]
fn a_file_that_declares_its_own_closed_range_declines() {
    expect_native_decline(
        "class Odd(override val start: Int, override val endInclusive: Int) : ClosedRange<Int> {\n\
         \x20   override fun contains(value: Int): Boolean = value % 2 == 1\n\
         }\n\
         fun box(): String {\n\
         \x20   val range: ClosedRange<Int> = Odd(1, 9)\n\
         \x20   return if (3 in range && 4 !in range) \"OK\" else \"fail\"\n\
         }\n",
        "DeclaredClosedRange",
        "contains",
    );
}

/// A file that declares its OWN `Comparable` does not reach the runtime's range. The order the
/// runtime would read is the DESCRIPTOR's, and a class of the program's has no entry there — the
/// position `Comparable.compareTo` already takes. So the construction is handed back to the
/// general `rangeTo` path and declines by name, rather than being ordered by a table that cannot
/// order it. (`reference_range_expression_e2e::user_comparable_range_binds_and_contains` pins the
/// other half: where that general path IS answered, the program runs.)
#[test]
fn a_range_over_the_files_own_comparable_does_not_reach_the_runtimes() {
    expect_native_decline(
        "class V(val n: Int) : Comparable<V> {\n\
         \x20   override fun compareTo(other: V): Int = n.compareTo(other.n)\n\
         }\n\
         fun box(): String {\n\
         \x20   val range = V(1)..V(3)\n\
         \x20   return if (V(2) in range) \"OK\" else \"fail\"\n\
         }\n",
        "OwnComparableRange",
        "rangeTo",
    );
}
