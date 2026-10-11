//! Structural `==` between an `Int`/`Long`-like primitive and a reference compares without boxing,
//! as kotlinc's `PrimitiveToObjectComparison` does: both operands are evaluated in source order,
//! the reference is checked (`ifnonnull` on a box, `instanceof` otherwise), and only then unboxed.
//! A condition branches on the comparison directly instead of materializing a Boolean.
//!
//! A `Byte`/`Short` pair compares at `Int` through a safe conversion of the nullable operand, and an
//! equality of a safe call with a primitive folds into the call's null guard, so the other operand
//! is not evaluated for a `null` receiver.

use super::common;

const SOURCE: &str = "\
package store\n\
\n\
class Holder<T>(val value: T)\n\
\n\
fun sameLong(left: Long, right: Long?): Boolean = left == right\n\
\n\
fun pickLong(left: Long, right: Long?): String = if (left == right) \"same\" else \"other\"\n\
\n\
fun differentLong(left: Long, right: Long?): Boolean = left != right\n\
\n\
fun pickBoxedFirst(left: Int?, right: Int): String = if (left == right) \"same\" else \"other\"\n\
\n\
fun differentBoxedFirst(left: Long?, right: Long): Boolean = left != right\n\
\n\
fun sameAny(left: Int, right: Any?): Boolean = left == right\n\
\n\
fun pickChar(left: Char, right: Char?): String = if (left != right) \"other\" else \"same\"\n\
\n\
fun knownPresent(holder: Holder<Long>): String {\n\
    val expected: Long? = 6L\n\
    return if (holder.value == expected) \"same\" else \"other\"\n\
}\n\
\n\
fun sameShort(left: Short, right: Short?): Boolean = left == right\n\
\n\
fun pickByteFirst(left: Byte?, right: Byte): String = if (left != right) \"other\" else \"same\"\n\
\n\
fun counted(): Int = 3\n\
\n\
fun sizeIs(text: String?): Boolean = text?.length == 3\n\
\n\
fun countedIs(text: String?): String = if (counted() != text?.length) \"other\" else \"same\"\n\
";

#[test]
fn primitive_to_object_equality_is_byte_identical_to_kotlinc() {
    common::byte_diff_against_kotlinc_cp(
        "PrimitiveToObjectEquality",
        SOURCE,
        "store/PrimitiveToObjectEqualityKt",
        &[common::stdlib_jar()],
    )
    .expect("reference kotlinc is provisioned")
    .unwrap_or_else(|diff| {
        panic!("store/PrimitiveToObjectEqualityKt differs from kotlinc: {diff}")
    });
}
