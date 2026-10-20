//! Membership in an unsigned range literal is kotlinc's `RangeContainsLowering`: `c in a..b` is
//! `0 <= c.compareTo(a) && c.compareTo(b) <= 0`, the value the receiver of each ordering. The
//! unsigned `compareTo` is an inline function whose body calls `UnsignedKt.uintCompare` /
//! `ulongCompare`, so each ordering is that call, and it ends an inlined call: inside the
//! condition the line is written again at the jump that consumes it, and the second comparison,
//! built at the `in` expression's line, marks that line at its jump. A plain `<` or an explicit
//! `compareTo` call is kotlinc's unsigned-compare intrinsic, `Integer.compareUnsigned`, and is
//! unaffected.
use super::common;

#[test]
fn unsigned_range_membership_orders_through_the_inlined_compare_to_like_kotlinc() {
    let source = "\
fun next(): UInt = 3u

fun inclusive(a: UInt, b: UInt, c: UInt): Boolean = c in a..b

fun exclusive(a: UInt, b: UInt, c: UInt): Boolean = c !in a..<b

fun infix(a: UInt, b: UInt, c: UInt): Boolean = c in a until b

fun long(a: ULong, b: ULong, c: ULong): Boolean = c in a..b

fun constant(c: UInt): Boolean = c in 1u..10u

fun evaluated(a: UInt, b: UInt): Boolean {
    val inside = next() in a..b
    return inside
}

fun branch(a: UInt, b: UInt, c: UInt): Int {
    if (c in a..b) return 1
    if (c !in 5u..<9u) return 2
    return when {
        c in a until b -> 3
        else -> 4
    }
}

fun ordered(a: UInt, b: UInt, x: ULong, y: ULong): Boolean = a < b && x >= y && a.compareTo(b) == 0
";
    common::byte_diff_against_kotlinc_cp(
        "UnsignedRangeContains",
        source,
        "UnsignedRangeContainsKt",
        &[common::stdlib_jar()],
    )
    .expect("the reference kotlinc is provisioned")
    .unwrap_or_else(|why| panic!("{why}"));
}
