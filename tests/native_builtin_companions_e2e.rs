//! `Int.Companion` and its relatives: the companion object of a BUILT-IN type.
//!
//! Each is declared in no file krusty compiles and carries no state — every member of one is a
//! constant the frontend folds — so the only thing a program can observe about it is its IDENTITY,
//! which the corpus asks directly (`o === Int.Companion`). The runtime holds one static object per
//! companion, each with a descriptor of its own so that `Int.Companion === Long.Companion` is false.
//!
//! Beside them, the defect these cases uncovered: a `Byte`-typed constant was recorded in common IR
//! as an `Int`, because `FirConstant` has no narrower integral case and the checked type was the
//! only thing carrying the width. A backend that boxes by the constant's SHAPE then boxes
//! `Byte.MIN_VALUE` as an `Int` — so `Byte.MIN_VALUE as Any is Byte` answered false, and two equal
//! bytes compared unequal once boxed.
//!
//! Every expectation is kotlinc's, taken by running the same program under it.

use super::common::{expect_box_ok_with_stdlib, expect_native_box};

/// A constant keeps its own WIDTH when it is boxed — the defect these cases uncovered.
#[test]
fn a_narrow_constant_boxes_as_its_own_type() {
    let source = "fun box(): String {\n\
         \x20   val byte: Any = Byte.MIN_VALUE\n\
         \x20   if (byte !is Byte) return \"fail byte is\"\n\
         \x20   if (byte is Int) return \"fail byte is an int\"\n\
         \x20   val short: Any = Short.MAX_VALUE\n\
         \x20   if (short !is Short) return \"fail short is\"\n\
         \x20   // A literal with a declared narrow type behaves the same way.\n\
         \x20   val written: Byte = -128\n\
         \x20   val boxed: Any = written\n\
         \x20   if (boxed !is Byte) return \"fail written\"\n\
         \x20   if (boxed != byte) return \"fail equal\"\n\
         \x20   // The wide ones are unchanged.\n\
         \x20   val int: Any = Int.MIN_VALUE\n\
         \x20   if (int !is Int) return \"fail int is\"\n\
         \x20   val long: Any = Long.MAX_VALUE\n\
         \x20   if (long !is Long) return \"fail long is\"\n\
         \x20   return \"OK\"\n\
         }\n";
    expect_box_ok_with_stdlib(source, "NarrowConstantWidth");
    expect_native_box(source, "NarrowConstantWidth", "OK");
}

/// Narrow constants still do arithmetic at `Int`, as Kotlin specifies.
#[test]
fn a_narrow_constant_still_promotes_for_arithmetic() {
    let source = "fun box(): String {\n\
         \x20   val a: Byte = 100\n\
         \x20   val b: Byte = 27\n\
         \x20   // `Byte + Byte` is an `Int` in Kotlin, so this does not overflow.\n\
         \x20   if (a + b != 127) return \"fail sum\"\n\
         \x20   if (a + 28 != 128) return \"fail past the maximum\"\n\
         \x20   val s: Short = 32767\n\
         \x20   if (s + 1 != 32768) return \"fail short\"\n\
         \x20   if (Byte.MAX_VALUE + 1 != 128) return \"fail constant sum\"\n\
         \x20   if (Byte.MIN_VALUE.toInt() != -128) return \"fail toInt\"\n\
         \x20   return \"OK\"\n\
         }\n";
    expect_box_ok_with_stdlib(source, "NarrowConstantArithmetic");
    expect_native_box(source, "NarrowConstantArithmetic", "OK");
}
