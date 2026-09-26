//! An unsigned counted loop marks lines around the calls kotlinc inlines, as kotlinc does.
//!
//! kotlinc's `ForLoopsLowering` converts a non-constant `UInt`/`ULong` bound to its `Int`/`Long`
//! representation with the inline `toInt()`/`toLong()` and orders the counter with the inline
//! `compareTo`. After an inlined call codegen forgets the line in effect, so the next mark is
//! written again — the store of `last` gets the loop's line (`line 7: 3`) — and inside a condition
//! it writes the line at once, for the jump. krusty realized both calls in place with no such
//! marks.
use super::common;

/// Literal, `until` and `downTo` bounds, a progression value, and stepped headers, whose
/// `getProgressionLastElement` takes the bounds converted and returns a value that needs none.
const SOURCE: &str = "package store\n\
    \n\
    fun sink(x: UInt) {}\n\
    fun sinkLong(x: ULong) {}\n\
    \n\
    fun closed(n: UInt) {\n\
    \x20   for (i in 1u..n) {\n\
    \x20       sink(i)\n\
    \x20   }\n\
    }\n\
    \n\
    fun bounds(from: UInt, to: UInt) {\n\
    \x20   for (i in from until to) {\n\
    \x20       sink(i)\n\
    \x20   }\n\
    }\n\
    \n\
    fun down(from: ULong, to: ULong) {\n\
    \x20   for (i in from downTo to) {\n\
    \x20       sinkLong(i)\n\
    \x20   }\n\
    }\n\
    \n\
    fun values(p: UIntProgression) {\n\
    \x20   for (i in p) {\n\
    \x20       sink(i)\n\
    \x20   }\n\
    }\n\
    \n\
    fun stepped(n: UInt) {\n\
    \x20   for (i in 1u..n step 2) {\n\
    \x20       sink(i)\n\
    \x20   }\n\
    }\n\
    \n\
    fun twiceStepped(n: ULong) {\n\
    \x20   for (i in 1uL..n step 2L step 3L) {\n\
    \x20       sinkLong(i)\n\
    \x20   }\n\
    }\n";

#[test]
fn an_unsigned_counted_loop_marks_lines_after_inlined_conversions_like_kotlinc() {
    common::byte_diff_against_kotlinc_cp(
        "UnsignedLoopLines",
        SOURCE,
        "store/UnsignedLoopLinesKt",
        &[common::stdlib_jar()],
    )
    .expect("reference kotlinc is provisioned")
    .unwrap_or_else(|difference| panic!("{difference}"));
}
