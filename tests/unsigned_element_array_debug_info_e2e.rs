//! An `Array<UInt>` stores `kotlin.UInt` boxes, so its descriptor is `[Lkotlin/UInt;` everywhere,
//! debug information included, as kotlinc writes it.
//!
//! The box class `kotlin/UInt` shares its name with the semantic scalar, so describing the boxed
//! element by that name described the unboxed `int` instead: `[I`. Method descriptors already took
//! the reference-slot path; the local-variable rows did not. A constructor typed its `raw` local
//! `[I`, and every generated value-class member over such an array lost its `LocalVariableTable`,
//! whose method descriptor then named no method of the class.

use super::common;

const DECLARATIONS: &str = r#"@JvmInline
value class Uns(val raw: Array<UInt>)
@JvmInline
value class Longs(val raw: Array<ULong>)
@JvmInline
value class NUns(val raw: Array<UInt>?)
@JvmInline
value class Shorts(val raw: Array<UShort?>)
class Holder(val raw: Array<UInt>, val bytes: Array<UByte>?)
fun take(a: Array<UInt>, b: Array<UByte>?): Int = a.size + (b?.size ?: 0)
"#;

const BOX: &str = r#"
fun box(): String {
    val texts = listOf(
        Uns(arrayOf(1u, 4000000000u)).toString(),
        Longs(arrayOf(18446744073709551615uL)).toString(),
        NUns(null).toString(),
        Shorts(arrayOf(65535u, null)).toString(),
        Holder(arrayOf(2u), arrayOf(3.toUByte())).raw[0].toString(),
        take(arrayOf(1u, 2u), null).toString(),
    ).joinToString(";")
    val expected = "Uns(raw=[1, 4000000000]);Longs(raw=[18446744073709551615]);NUns(raw=null);" +
        "Shorts(raw=[65535, null]);2;2"
    return if (texts == expected) "OK" else texts
}
"#;

#[test]
fn an_array_of_unsigned_values_is_described_by_its_boxes_like_kotlinc() {
    common::assert_classes_identical_to_kotlinc(
        "UnsignedElementArrays",
        DECLARATIONS,
        &[
            "Uns",
            "Longs",
            "NUns",
            "Shorts",
            "Holder",
            "UnsignedElementArraysKt",
        ],
    );
}

#[test]
fn an_array_of_unsigned_values_runs_like_kotlinc() {
    common::expect_box_same_as_kotlinc(
        &format!("{DECLARATIONS}{BOX}"),
        "unsigned_element_array_debug_info",
    );
}
