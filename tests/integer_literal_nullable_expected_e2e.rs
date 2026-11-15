//! The stdlib collection forms of the shared `integerLiteralNullableExpected` program: the
//! covariant `List<out E>` and `Set<out E>` take an integer or unsigned literal against a nullable
//! expectation exactly as the program's own `Box<out T>` does. Native and Wasm decline the stdlib
//! `listOf` body, so only the JVM runs these.

use super::common;

#[test]
fn a_literal_next_to_null_takes_a_nullable_collection_element_type() {
    let source = "\
fun box(): String {
    val listed: List<UShort?> = listOf(1u, null)
    if (listed.size != 2 || listed[0]!!.toInt() != 1 || listed[1] != null) return \"listOf\"
    val reversed: List<Long?> = listOf(null, 2)
    if (reversed[0] != null || reversed[1] != 2L) return \"reversed\"
    val set: Set<Short?> = setOf(3, null)
    if (!set.contains(3.toShort()) || !set.contains(null)) return \"setOf\"
    return \"OK\"
}
";
    common::expect_box_same_as_kotlinc(source, "integer_literal_nullable_expected");
}
