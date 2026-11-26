//! `kotlin.collections.arrayFill`, the `@GCUnsafeCall` external the library's own array `fill`
//! bodies end in: one entry per specialized array and one for `Array<T>`.
//!
//! Every expectation is kotlinc's, taken by running the same program under it.

use super::common::{expect_box_ok_with_stdlib, expect_native_box, kotlinc_box_result};

fn every_backend_agrees_with_kotlinc(stem: &str, source: &str) {
    assert_eq!(
        kotlinc_box_result(source),
        "OK",
        "{stem}: unexpected kotlinc result"
    );
    expect_box_ok_with_stdlib(source, stem);
    expect_native_box(source, stem, "OK");
}

/// Each array kind writes its own element to the range and leaves the rest alone.
#[test]
fn every_array_kind_fills_the_range_it_is_given() {
    let source = "fun box(): String {\n\
         \x20   val bytes = ByteArray(4); bytes.fill(7, 1, 3)\n\
         \x20   if (bytes.toList() != listOf<Byte>(0, 7, 7, 0)) return \"fail byte\"\n\
         \x20   val shorts = ShortArray(3); shorts.fill(-2)\n\
         \x20   if (shorts.toList() != listOf<Short>(-2, -2, -2)) return \"fail short\"\n\
         \x20   val chars = CharArray(3); chars.fill('x', 2, 3)\n\
         \x20   if (chars.toList() != listOf('\\u0000', '\\u0000', 'x')) return \"fail char\"\n\
         \x20   val ints = IntArray(3); ints.fill(5, 0, 2)\n\
         \x20   if (ints.toList() != listOf(5, 5, 0)) return \"fail int\"\n\
         \x20   val longs = LongArray(2); longs.fill(1L shl 40)\n\
         \x20   if (longs.toList() != listOf(1L shl 40, 1L shl 40)) return \"fail long\"\n\
         \x20   val floats = FloatArray(2); floats.fill(1.5f, 1, 2)\n\
         \x20   if (floats.toList() != listOf(0f, 1.5f)) return \"fail float\"\n\
         \x20   val doubles = DoubleArray(2); doubles.fill(-0.25)\n\
         \x20   if (doubles.toList() != listOf(-0.25, -0.25)) return \"fail double\"\n\
         \x20   val booleans = BooleanArray(3); booleans.fill(true, 1, 3)\n\
         \x20   if (booleans.toList() != listOf(false, true, true)) return \"fail boolean\"\n\
         \x20   val refs = arrayOfNulls<String>(3); refs.fill(\"k\", 0, 2)\n\
         \x20   if (refs.toList() != listOf<String?>(\"k\", \"k\", null)) return \"fail reference\"\n\
         \x20   return \"OK\"\n\
         }\n";
    every_backend_agrees_with_kotlinc("ArrayFillKinds", source);
}

/// A range outside the array raises before anything is written.
#[test]
fn a_range_outside_the_array_raises_and_writes_nothing() {
    let source = "fun box(): String {\n\
         \x20   val ints = IntArray(3)\n\
         \x20   try { ints.fill(1, 2, 5); return \"fail past the end\" } catch (e: IndexOutOfBoundsException) {}\n\
         \x20   try { ints.fill(1, -1, 2); return \"fail before the start\" } catch (e: IndexOutOfBoundsException) {}\n\
         \x20   try { ints.fill(1, 2, 1); return \"fail reversed\" } catch (e: IllegalArgumentException) {}\n\
         \x20   if (ints.toList() != listOf(0, 0, 0)) return \"fail written\"\n\
         \x20   return \"OK\"\n\
         }\n";
    every_backend_agrees_with_kotlinc("ArrayFillRange", source);
}
