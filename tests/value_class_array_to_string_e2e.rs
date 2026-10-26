//! A value class over an array renders the array's contents, as kotlinc's generated `toString`
//! does: `Bits(raw=[1, 2])` through `Arrays.toString`, not the array's identity (`[I@1b6d3586`).
//!
//! The overload follows the property's declared type: a primitive array takes its own
//! (`([I)`, `([J)`), and every reference array, nullable or not and whatever its element, takes
//! `([Ljava/lang/Object;)`. A nullable array takes it too, and `Arrays.toString` renders `null`.
//! An unsigned array is itself a value class, so it renders through `UIntArray.toString-impl`
//! (`UBits(raw=UIntArray(storage=[1, -294967296]))`), not through `Arrays.toString` of its
//! carrier. `hashCode` already used `Arrays.hashCode` and `equals` compares the arrays by
//! reference, both as kotlinc does; the byte comparison of each whole class covers them too.

use super::common;

const DECLARATIONS: &str = r#"@file:OptIn(ExperimentalUnsignedTypes::class)

@JvmInline
value class Bits(val raw: IntArray)
@JvmInline
value class Longs(val raw: LongArray)
@JvmInline
value class NBits(val raw: IntArray?)
@JvmInline
value class Names(val raw: Array<String>)
@JvmInline
value class NNames(val raw: Array<Int>?)
@JvmInline
value class UBits(val raw: UIntArray)
@JvmInline
value class Uns(val raw: Array<UInt>)
"#;

const BOX: &str = r#"
fun box(): String {
    val shared = intArrayOf(1, 2)
    val texts = listOf(
        Bits(shared).toString(),
        Longs(longArrayOf(3L)).toString(),
        NBits(intArrayOf(4)).toString(),
        NBits(null).toString(),
        Names(arrayOf("a", "b")).toString(),
        NNames(arrayOf(5)).toString(),
        NNames(null).toString(),
        UBits(uintArrayOf(1u, 4000000000u)).toString(),
        Uns(arrayOf(6u)).toString(),
        "in ${Bits(shared)}",
        (Bits(shared).hashCode() == shared.contentHashCode()).toString(),
        (Bits(shared) == Bits(shared)).toString(),
        (Bits(shared) == Bits(intArrayOf(1, 2))).toString(),
    ).joinToString(";")
    val expected = "Bits(raw=[1, 2]);Longs(raw=[3]);NBits(raw=[4]);NBits(raw=null);" +
        "Names(raw=[a, b]);NNames(raw=[5]);NNames(raw=null);UBits(raw=UIntArray(storage=[1, -294967296]));" +
        "Uns(raw=[6]);in Bits(raw=[1, 2]);true;true;false"
    return if (texts == expected) "OK" else texts
}
"#;

#[test]
fn a_value_class_over_an_array_renders_its_contents_like_kotlinc() {
    common::assert_classes_identical_to_kotlinc(
        "ArrayValueClasses",
        DECLARATIONS,
        &["Bits", "Longs", "NBits", "Names", "NNames", "UBits", "Uns"],
    );
}

#[test]
fn a_value_class_over_an_array_prints_like_kotlinc() {
    common::expect_box_same_as_kotlinc(
        &format!("{DECLARATIONS}{BOX}"),
        "value_class_array_to_string",
    );
}
