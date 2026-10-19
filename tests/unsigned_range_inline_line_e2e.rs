//! `UInt.rangeTo`/`rangeUntil` and their `ULong` counterparts are inline functions that kotlinc
//! inlines, so the range construction that realizes one in place ends an inlined call
//! (`markLineNumberAfterInlineIfNeeded`): the caller's line is forgotten after it and written
//! again at the next instruction that marks one, such as the `astore` of `val r = a..b`. The end
//! comes from the selected declaration's metadata `inline` flag: the non-inline `until` and the
//! builtin `Int` range operators end no inlined call, and a `for` loop over a range literal never
//! constructs the range. Membership through a user `contains` that keeps the range, and an
//! implicit-receiver `rangeTo` call, build the same construction and end an inlined call too.
use super::common;

#[test]
fn an_unsigned_range_operator_ends_an_inlined_call_like_kotlinc() {
    let source = "\
fun sink() {}

fun values(a: UInt, b: UInt, x: ULong, y: ULong): String {
    val through = a..b
    val openEnd = a..<b
    val infix = a until b
    val longThrough = x..y
    val longOpenEnd = x..<y
    val signed = 1..<5
    return \"$through $openEnd $infix $longThrough $longOpenEnd $signed\"
}

fun discarded(a: UInt, b: UInt) {
    a..b; sink()
}

fun condition(a: UInt, b: UInt): Boolean {
    if ((a..<b).isEmpty()) return true
    return (a..b).isEmpty() && b > a
}

fun stepped(a: UInt, b: UInt): UInt {
    var sum = 0u
    val range = a..<b
    for (i in range step 2) {
        sum += i
    }
    for (i in a..b step 3) {
        sum += i
    }
    for (i in (a..b).reversed()) {
        sum += i
    }
    return sum
}

fun argument(a: ULong, b: ULong): Int = count(a..b, a..<b)

fun count(r: ULongRange, s: ULongRange): Int = if (r.isEmpty()) 0 else if (s.isEmpty()) 1 else 2
";
    common::byte_diff_against_kotlinc_cp(
        "UnsignedRangeLines",
        source,
        "UnsignedRangeLinesKt",
        &[common::stdlib_jar()],
    )
    .expect("the reference kotlinc is provisioned")
    .unwrap_or_else(|why| panic!("{why}"));
}

/// The two checker paths that build a range construction outside the operator form: membership
/// through a user `contains` that keeps the range as its receiver, and an implicit-receiver
/// `rangeTo`/`rangeUntil` call inside a lambda with a `UInt` receiver.
#[test]
fn a_kept_membership_range_and_an_implicit_receiver_call_end_an_inlined_call_like_kotlinc() {
    let source = "\
operator fun UIntRange.contains(value: Long): Boolean = value == first.toLong()

fun custom(a: UInt, b: UInt, c: Long): Boolean {
    val inside = c in a..b
    return inside
}

fun customCondition(a: UInt, b: UInt, c: Long): Int {
    if (c !in a..<b) return 1
    return 2
}

fun build(a: UInt, f: UInt.() -> UIntRange): UIntRange = a.f()

fun viaLambda(a: UInt, b: UInt): UIntRange = build(a) {
    val r = rangeTo(b)
    r
}

fun viaRun(a: UInt, b: UInt): UIntRange {
    val r = a.run {
        val s = rangeUntil(b)
        s
    }
    return r
}
";
    common::byte_diff_against_kotlinc_cp(
        "UnsignedRangeFormsLines",
        source,
        "UnsignedRangeFormsLinesKt",
        &[common::stdlib_jar()],
    )
    .expect("the reference kotlinc is provisioned")
    .unwrap_or_else(|why| panic!("{why}"));
}
