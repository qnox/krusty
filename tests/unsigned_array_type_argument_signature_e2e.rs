//! An unsigned array as an array element or a type argument is its value class, as kotlinc signs it.
//!
//! `UIntArray` is a value class over `IntArray`. Where it is a value of its own it is carried as
//! the carrier (`[I`), but an `Array<UIntArray>` element or a `List<UIntArray>` argument holds the
//! box, so the generic `Signature` spells the class: `(Ljava/util/List<Lkotlin/UIntArray;>;)` and
//! `[Lkotlin/UIntArray;`. A signature that then adds nothing over the descriptor (every array
//! position here) is not written at all. Spelling the carrier instead (`[[I`, `List<[I>`) names a
//! different JVM type than the descriptor and than the elements the array holds.

use super::common;

const DECLARATIONS: &str = r#"@file:OptIn(ExperimentalUnsignedTypes::class)

class Holder(val grid: Array<UIntArray>, var bytes: Array<UByteArray>?) {
    val longs: Array<ULongArray> = arrayOf(ulongArrayOf(9uL))
}

fun first(back: Array<UIntArray>): UIntArray = back[0]
fun shorts(xs: Array<UShortArray>): Array<UShortArray> = xs
fun nullable(xs: Array<UIntArray?>): UIntArray? = xs[0]
fun listed(xs: List<UIntArray>): UIntArray = xs[0]
fun mapped(m: Map<String, Array<UByteArray>>): Int = m.size
fun deep(xs: Array<Array<UIntArray>>): Int = xs[0][0].size
fun make(seed: UIntArray): Array<UIntArray> = arrayOf(seed, seed)
"#;

const BOX: &str = r#"
fun box(): String {
    val holder = Holder(arrayOf(uintArrayOf(4u)), null)
    holder.bytes = arrayOf(ubyteArrayOf(255u))
    val texts = listOf(
        first(holder.grid)[0].toString(),
        shorts(arrayOf(ushortArrayOf(65535u)))[0][0].toString(),
        nullable(arrayOf(null)).toString(),
        listed(listOf(uintArrayOf(7u)))[0].toString(),
        mapped(mapOf("a" to arrayOf(UByteArray(1)))).toString(),
        deep(arrayOf(arrayOf(UIntArray(3)))).toString(),
        make(UIntArray(2))[1].size.toString(),
        holder.bytes!![0][0].toString(),
        holder.longs[0][0].toString(),
    ).joinToString(",")
    return if (texts == "4,65535,null,7,1,3,2,255,9") "OK" else texts
}
"#;

#[test]
fn an_unsigned_array_type_argument_is_signed_as_its_class_like_kotlinc() {
    common::assert_classes_identical_to_kotlinc(
        "UnsignedTypeArguments",
        DECLARATIONS,
        &["UnsignedTypeArgumentsKt", "Holder"],
    );
}

#[test]
fn an_unsigned_array_type_argument_runs_like_kotlinc() {
    common::expect_box_same_as_kotlinc(
        &format!("{DECLARATIONS}{BOX}"),
        "unsigned_array_type_arguments",
    );
}
