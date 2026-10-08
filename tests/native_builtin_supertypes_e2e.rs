//! `is Number` and `is Comparable<*>`, the two built-in supertypes a value type has.
//!
//! Neither has instances of its own — every value that is one is a boxed primitive or a string — so
//! each is a descriptor the boxes point at rather than a type anything wears. That is the whole of
//! the realization: the interface list on each box, flattened and transitive as the runtime's
//! `is` requires, and a name at the check.
//!
//! Which box points at which is Kotlin's asymmetry and not a tidy rule: `Char` and `Boolean` are
//! `Comparable` and NOT `Number`, and an unsigned integer is `Comparable` and not `Number` either —
//! it is a value class rather than a `java.lang.Number`. Every expectation below is the reference
//! compiler's answer, asked of it directly.

use super::common::{expect_box_ok_with_stdlib, expect_native_box};

#[test]
fn every_numeric_box_is_a_number_and_nothing_else_is() {
    // `Char` and `Boolean` are the two that look numeric and are not, so they are the point of the
    // program rather than padding: a rule keyed on "carried in a machine word" would get both wrong.
    let src = "fun box(): String {\n\
               \x20   val values: List<Any> = listOf(1.toByte(), 1.toShort(), 1, 1L, 1.0f, 1.0)\n\
               \x20   var numbers = 0\n\
               \x20   for (value in values) if (value is Number) numbers++\n\
               \x20   val ch: Any = 'a'\n\
               \x20   val flag: Any = true\n\
               \x20   val text: Any = \"x\"\n\
               \x20   if (ch is Number) return \"fail: Char is Number\"\n\
               \x20   if (flag is Number) return \"fail: Boolean is Number\"\n\
               \x20   if (text is Number) return \"fail: String is Number\"\n\
               \x20   return if (numbers == 6) \"OK\" else \"fail: $numbers\"\n\
               }\n";
    expect_box_ok_with_stdlib(src, "EveryNumericBoxIsANumber");
    expect_native_box(src, "EveryNumericBoxIsANumber", "OK");
}

#[test]
fn a_value_type_is_comparable_even_when_it_is_not_a_number() {
    let src = "fun box(): String {\n\
               \x20   val n: Any = 1\n\
               \x20   val ch: Any = 'a'\n\
               \x20   val flag: Any = true\n\
               \x20   val text: Any = \"x\"\n\
               \x20   if (n !is Comparable<*>) return \"fail: Int\"\n\
               \x20   if (ch !is Comparable<*>) return \"fail: Char\"\n\
               \x20   if (flag !is Comparable<*>) return \"fail: Boolean\"\n\
               \x20   if (text !is Comparable<*>) return \"fail: String\"\n\
               \x20   return \"OK\"\n\
               }\n";
    expect_box_ok_with_stdlib(src, "ValueTypesAreComparable");
    expect_native_box(src, "ValueTypesAreComparable", "OK");
}

#[test]
fn an_ordinary_class_is_neither_unless_it_says_so() {
    // The interface list is per type, so a class that declares nothing must answer false to both —
    // otherwise the boxes' lists would be leaking through the shared `kotlin.Any` super.
    let src = "class Plain\n\
               fun box(): String {\n\
               \x20   val plain: Any = Plain()\n\
               \x20   if (plain is Number) return \"fail: Number\"\n\
               \x20   if (plain is Comparable<*>) return \"fail: Comparable\"\n\
               \x20   return \"OK\"\n\
               }\n";
    expect_box_ok_with_stdlib(src, "PlainClassIsNeither");
    expect_native_box(src, "PlainClassIsNeither", "OK");
}

#[test]
fn an_unsigned_integer_is_comparable_but_not_a_number() {
    // The asymmetry stated on its own: an unsigned integer is a value class, so it is `Comparable`
    // and not a `Number`, which is the reference compiler's answer rather than my reading.
    let src = "fun box(): String {\n\
               \x20   val u: Any = 1u\n\
               \x20   val ul: Any = 1uL\n\
               \x20   if (u is Number) return \"fail: UInt is Number\"\n\
               \x20   if (u !is Comparable<*>) return \"fail: UInt is not Comparable\"\n\
               \x20   if (ul !is Comparable<*>) return \"fail: ULong is not Comparable\"\n\
               \x20   return \"OK\"\n\
               }\n";
    expect_box_ok_with_stdlib(src, "UnsignedIsComparableNotNumber");
    expect_native_box(src, "UnsignedIsComparableNotNumber", "OK");
}
