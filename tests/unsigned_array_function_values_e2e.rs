//! A function value over an unsigned array class (`UIntArray`) and a reference to an unsigned array
//! factory (`::uintArrayOf`). The class and its signed carrier share one descriptor, so each
//! boundary — the invocation's erased result, a reference carrier's `invoke` bridge — has to move
//! the box, not the carrier. The unsigned factories are ordinary stdlib functions
//! (`kotlin.UIntArrayKt`), not compiler-provided ones, so a reference to one is a stdlib reference.

use super::common;

#[test]
fn a_lambda_over_an_unsigned_array_returns_its_box() {
    common::expect_box_same_as_kotlinc(
        "\
@file:OptIn(ExperimentalUnsignedTypes::class)
fun box(): String {
    val f: (UIntArray) -> UIntArray = { it }
    val r = f(uintArrayOf(42u))
    return if (r[0] == 42u) \"OK\" else \"fail \" + r[0]
}
",
        "UnsignedArrayLambda",
    );
}

#[test]
fn a_function_reference_over_an_unsigned_array_unboxes_its_argument_and_boxes_its_result() {
    common::expect_box_same_as_kotlinc(
        "\
@file:OptIn(ExperimentalUnsignedTypes::class)
fun id(a: UIntArray) = a
fun box(): String {
    val f = ::id
    val r = f(uintArrayOf(42u))
    return if (r[0] == 42u) \"OK\" else \"fail \" + r[0]
}
",
        "UnsignedArrayReference",
    );
}

#[test]
fn references_to_unsigned_array_factories_run() {
    common::expect_box_same_as_kotlinc(
        "\
@file:OptIn(ExperimentalUnsignedTypes::class)
fun box(): String {
    val fn = ::uintArrayOf
    val whole = fn(uintArrayOf(42u))
    if (whole[0] != 42u) return \"fail whole\"

    val adapted: (ULong, ULong) -> ULongArray = ::ulongArrayOf
    val a = adapted(1uL, 2uL)
    if (!a.contentEquals(ulongArrayOf(1uL, 2uL))) return \"fail adapted \" + a.contentToString()

    val bytes: (UByte, UByte) -> UByteArray = ::ubyteArrayOf
    if (!bytes(3u, 4u).contentEquals(ubyteArrayOf(3u, 4u))) return \"fail ubyte\"
    val shorts: (UShort) -> UShortArray = ::ushortArrayOf
    if (shorts(5u).size != 1) return \"fail ushort\"
    return \"OK\"
}
",
        "UnsignedArrayFactoryReferences",
    );
}
