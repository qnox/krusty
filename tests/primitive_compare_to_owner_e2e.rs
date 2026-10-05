//! An explicit primitive `compareTo` calls the method kotlinc's `CompareTo` intrinsic names.
//!
//! The int category (`Int`, `Char`, `Short`, `Byte`) and `Long` go through
//! `kotlin/jvm/internal/Intrinsics.compare`, `Boolean` through `java/lang/Boolean.compare(ZZ)`, and
//! the floating types through their wrappers. Mixed operands compare at the promoted type. A
//! `Boolean` ordering, explicit or written as `<`, stays that call tested against zero, since
//! kotlinc's comparison intrinsics cover only numbers; an `Int` result tested against zero still
//! folds into `if_icmpXX`.
use super::common;

const SOURCE: &str = "package store\n\
                      \n\
                      fun ints(x: Int, y: Int): Int = x.compareTo(y)\n\
                      fun longs(x: Long, y: Long): Int = x.compareTo(y)\n\
                      fun chars(x: Char, y: Char): Int = x.compareTo(y)\n\
                      fun shorts(x: Short, y: Short): Int = x.compareTo(y)\n\
                      fun bytes(x: Byte, y: Byte): Int = x.compareTo(y)\n\
                      fun flags(x: Boolean, y: Boolean): Int = x.compareTo(y)\n\
                      fun floats(x: Float, y: Float): Int = x.compareTo(y)\n\
                      fun doubles(x: Double, y: Double): Int = x.compareTo(y)\n\
                      fun widened(x: Int, y: Long): Int = x.compareTo(y)\n\
                      fun after(x: Int, y: Int): Boolean = x.compareTo(y) > 0\n\
                      fun flagLess(x: Boolean, y: Boolean): Boolean = x < y\n\
                      fun flagBranch(x: Boolean, y: Boolean): Int = if (x.compareTo(y) > 0) 1 else 2\n\
                      fun charLess(x: Char, y: Char): Boolean = x < y\n";

#[test]
fn primitive_compare_to_calls_kotlinc_intrinsic_owner() {
    common::byte_diff_against_kotlinc_cp(
        "PrimitiveCompareToOwner",
        SOURCE,
        "store/PrimitiveCompareToOwnerKt",
        &[common::stdlib_jar()],
    )
    .expect("reference kotlinc is provisioned")
    .unwrap_or_else(|diff| panic!("store/PrimitiveCompareToOwnerKt differs from kotlinc: {diff}"));
}
