//! `x.compareTo(y)` where the static type says only `Comparable`.
//!
//! The receiver's DESCRIPTOR says what to compare, exactly as `equals` and `toString` on such a
//! receiver already read it: a boxed primitive at its own width, a string by UTF-16 unit, the
//! unsigned integers read unsigned. Kotlin's order on the floating types is TOTAL, where the
//! machine's `<` is not — `-0.0` sits below `0.0` and every NaN above everything — and that is the
//! order `Comparable<Double>` answers with, which is the whole point of the corpus's `ieee754`
//! cases: the same two values compared as primitives are equal.
//!
//! A file that declares a `Comparable` of its own keeps declining. An object of the program's could
//! stand behind that type too, and no static type tells it from one the runtime made, which is why
//! the answer is the runtime's at all. An ENUM counts as one with nothing written: `kotlin.Enum`
//! supplies the comparison, whose ordinal is a field this generator lays out and the runtime cannot
//! read.
//!
//! Every expectation is kotlinc's, taken by running the same program under it.

use super::common::{expect_box_ok_with_stdlib, expect_native_box};

/// A boxed primitive and a string, compared through the type that says nothing more.
#[test]
fn a_comparable_receiver_is_ordered_by_what_its_descriptor_says_it_is() {
    let source = "fun box(): String {\n\
         \x20   val one: Any = 1\n\
         \x20   val more: Any = 42\n\
         \x20   if ((one as Comparable<Any>).compareTo(more) != -1) return \"fail ints\"\n\
         \x20   if ((more as Comparable<Any>).compareTo(one) != 1) return \"fail the other way\"\n\
         \x20   if ((one as Comparable<Any>).compareTo(one) != 0) return \"fail equal\"\n\
         \x20   val text: Comparable<String> = \"a\"\n\
         \x20   if (text >= \"b\") return \"fail text\"\n\
         \x20   if (text.compareTo(\"a\") != 0) return \"fail same text\"\n\
         \x20   val long: Comparable<Long> = 5L\n\
         \x20   if (long >= 6L) return \"fail long\"\n\
         \x20   val letter: Comparable<Char> = 'a'\n\
         \x20   if (letter >= 'b') return \"fail char\"\n\
         \x20   return \"OK\"\n\
         }\n";
    expect_box_ok_with_stdlib(source, "ComparableDescriptor");
    expect_native_box(source, "ComparableDescriptor", "OK");
}

/// The floating order is TOTAL through `Comparable` and IEEE as a primitive — the same two values
/// answer differently, which is what makes this worth its own test.
#[test]
fn the_floating_order_through_comparable_is_kotlins_total_one() {
    let source = "fun box(): String {\n\
         \x20   if ((-0.0 as Comparable<Double>) >= 0.0) return \"fail minus zero\"\n\
         \x20   if ((-0.0F as Comparable<Float>) >= 0.0F) return \"fail minus zero float\"\n\
         \x20   // As PRIMITIVES the two are equal, so neither is below the other.\n\
         \x20   if (-0.0 < 0.0) return \"fail primitive\"\n\
         \x20   // Every NaN is above everything, itself included.\n\
         \x20   val nan: Comparable<Double> = Double.NaN\n\
         \x20   if (nan <= 1.0) return \"fail nan above\"\n\
         \x20   if (nan.compareTo(Double.NaN) != 0) return \"fail nan equals itself\"\n\
         \x20   // `==` on such a receiver is a question about equality, not about order.\n\
         \x20   if ((-0.0 as Comparable<Double>) == 0.0) return \"fail equality\"\n\
         \x20   return \"OK\"\n\
         }\n";
    expect_box_ok_with_stdlib(source, "ComparableFloating");
    expect_native_box(source, "ComparableFloating", "OK");
}

/// A generic bound reaches the same answer: `T : Comparable<T>` erases to a reference, and the
/// descriptor behind it is what the walk reads.
#[test]
fn a_comparable_type_parameter_is_ordered_the_same_way() {
    let source = "fun <T : Comparable<T>> smaller(a: T, b: T): T =\n\
         \x20   if (a.compareTo(b) < 0) a else b\n\
         fun box(): String {\n\
         \x20   if (smaller(2, 1) != 1) return \"fail ints\"\n\
         \x20   if (smaller(\"b\", \"a\") != \"a\") return \"fail text\"\n\
         \x20   if (smaller(2.5, 1.5) != 1.5) return \"fail doubles\"\n\
         \x20   return \"OK\"\n\
         }\n";
    expect_box_ok_with_stdlib(source, "ComparableBound");
    expect_native_box(source, "ComparableBound", "OK");
}
