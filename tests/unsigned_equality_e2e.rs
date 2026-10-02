//! `==` / `!=` of unsigned values follows the JVM slot of each operand.
//!
//! A non-null `UInt` parameter or property is the primitive carrier. `UInt?`, and a non-null
//! `UInt` that arrives through `FunctionN.invoke`, is the `kotlin/UInt` box. Comparing those as
//! `Integer.valueOf` against the box makes equal values compare false. kotlinc calls
//! `equals-impl` when the carrier is on the left, null-checks and unboxes when the box is on the
//! left, and uses `Intrinsics.areEqual` when both operands are boxes.

use super::common;

const DIRECT: &str = "\
fun boxOnLeft(a: UInt?, b: UInt) = a == b
fun carrierOnLeft(b: UInt, a: UInt?) = b == a
fun bothBoxes(a: UInt?, b: UInt?) = a == b
fun bothCarriers(a: UInt, b: UInt) = a == b
val nullableUInt: UInt? = 7u
val differentUInt: UInt? = 2u
val uInt: UInt = 7u
fun fieldsBoxOnLeft() = nullableUInt == uInt
fun fieldsCarrierOnLeft() = uInt == differentUInt
fun boxOnLeftNot(a: UInt?, b: UInt) = a != b
fun carrierOnLeftNot(b: UInt, a: UInt?) = b != a
fun ifCarrier(b: UInt, a: UInt?): String {
    if (b == a) return \"equal\"
    return \"different\"
}
fun ifBox(a: UInt?, b: UInt): String {
    if (a == b) return \"equal\"
    return \"different\"
}
fun narrow(a: UByte?, b: UByte) = a == b
fun wide(b: ULong, a: ULong?) = b == a
fun zero(a: UInt?) = a == 0u
fun box(): String {
    if (boxOnLeft(null, 0u)) return \"null box\"
    if (!boxOnLeft(2u, 2u)) return \"box value\"
    if (carrierOnLeft(0u, null)) return \"null carrier\"
    if (!carrierOnLeft(2u, 2u)) return \"carrier value\"
    if (!bothBoxes(2u, 2u) || bothBoxes(null, 2u) || !bothBoxes(null, null)) return \"boxes\"
    if (!bothCarriers(2u, 2u) || bothCarriers(2u, 3u)) return \"carriers\"
    if (!fieldsBoxOnLeft()) return \"field box\"
    if (fieldsCarrierOnLeft()) return \"field carrier\"
    if (!boxOnLeftNot(null, 0u) || boxOnLeftNot(2u, 2u)) return \"not box\"
    if (!carrierOnLeftNot(0u, null) || carrierOnLeftNot(2u, 2u)) return \"not carrier\"
    if (ifCarrier(2u, 2u) != \"equal\" || ifCarrier(2u, null) != \"different\") return \"if carrier\"
    if (ifBox(null, 0u) != \"different\" || ifBox(2u, 2u) != \"equal\") return \"if box\"
    if (narrow(null, 0u) || !narrow(2u, 2u)) return \"ubyte\"
    if (wide(0uL, null) || !wide(2uL, 2uL)) return \"ulong\"
    if (zero(null) || !zero(0u)) return \"zero\"
    val same = { arg: UInt -> 2u == arg }
    if (!same(2u)) return \"lambda carrier\"
    val flipped = { arg: UInt -> arg == 7u }
    if (!flipped(7u)) return \"lambda box\"
    val nullable = { arg: UInt? -> 7u == arg }
    if (!nullable(7u) || nullable(null)) return \"lambda nullable\"
    return \"OK\"
}
";

#[test]
fn unsigned_equality_matches_kotlinc_bytecode_and_result() {
    let comparison = common::compare_with_kotlinc_plugin(
        "UnsignedEquality",
        DIRECT,
        "UnsignedEqualityKt",
        &[common::stdlib_jar(), common::jdk_modules()],
        "21",
        &[],
    )
    .expect("reference kotlinc and javap are provisioned");
    for marker in [
        "boxOnLeft-",
        "carrierOnLeft-",
        "bothBoxes-",
        "bothCarriers-",
        "fieldsBoxOnLeft(",
        "fieldsCarrierOnLeft(",
        "boxOnLeftNot-",
        "carrierOnLeftNot-",
        "ifCarrier-",
        "ifBox-",
        "narrow-",
        "wide-",
        "zero-",
        "box$lambda$0",
        "box$lambda$1",
        "box$lambda$2",
    ] {
        let reference = common::method_instructions(&comparison.reference, marker);
        let krusty = common::method_instructions(&comparison.krusty, marker);
        assert_eq!(
            krusty, reference,
            "{marker}: instructions differ from kotlinc\nkrusty:\n{krusty:?}\nkotlinc:\n{reference:?}"
        );
    }
    common::expect_box_same_as_kotlinc(DIRECT, "UnsignedEquality");
}
