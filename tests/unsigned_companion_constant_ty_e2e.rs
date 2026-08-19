//! A classpath constant's type comes from the Kotlin METADATA, not the JVM field descriptor.
//!
//! `UInt.Companion.MIN_VALUE` is stored in an `int` field, so typing the read from the descriptor
//! made it `Int`. An ANNOTATED declaration hid that — the expected type coerced the read — but an
//! INFERRED one published `Int` into the field, the getter and the `@Metadata`, and any later use
//! that needed the real type was rejected: `val b = UInt.MIN_VALUE; val c: UInt = b` was an
//! "initializer type mismatch", and `listOf<UInt>(MaxUI)` matched no candidate.
use super::common;

#[test]
fn an_inferred_unsigned_constant_keeps_its_unsigned_type() {
    const SRC: &str = "val b = UInt.MIN_VALUE\n\
        val c: UInt = b\n\
        fun box(): String = if (c == 0u) \"OK\" else \"F\"\n";
    common::expect_box_ok_with_stdlib(SRC, "InferredUnsignedConstant");
}

#[test]
fn an_inferred_unsigned_constant_is_a_matching_type_argument() {
    const SRC: &str = "val max = UInt.MAX_VALUE\n\
        fun box(): String = if (listOf<UInt>(max).size == 1) \"OK\" else \"F\"\n";
    common::expect_box_ok_with_stdlib(SRC, "UnsignedConstantTypeArgument");
}

#[test]
fn a_signed_constant_is_unaffected() {
    // The control: a signed companion constant's metadata and descriptor agree, so nothing changes.
    const SRC: &str = "val b = Int.MAX_VALUE\n\
        val c: Int = b\n\
        fun box(): String = if (c == 2147483647) \"OK\" else \"F\"\n";
    common::expect_box_ok_with_stdlib(SRC, "SignedConstantControl");
}
