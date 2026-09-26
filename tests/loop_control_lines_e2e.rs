//! A `for` loop's generated control carries the loop's line, as kotlinc's does.
//!
//! kotlinc's `ForLoopsLowering` builds a `for` loop's update, exit test and bottom condition at the
//! loop's own offsets, and codegen marks them like any other expression: after the body, the
//! `for` line comes back at the first instruction of the update (`line 6: 11` after the body's
//! `line 7`). krusty emitted that control under the body's last line.
use super::common;

/// Closed, half-open and descending ranges, array elements, a `continue`, nested loops, and a body
/// on the loop's own line, where the mark deduplicates against the line already in effect.
const SOURCE: &str = "package store\n\
    \n\
    fun sink(x: Int) {}\n\
    \n\
    fun closed(n: Int) {\n\
    \x20   for (i in 0..n) {\n\
    \x20       sink(i)\n\
    \x20   }\n\
    }\n\
    \n\
    fun open(n: Int) {\n\
    \x20   for (i in 0 until n) {\n\
    \x20       sink(i)\n\
    \x20   }\n\
    }\n\
    \n\
    fun down(n: Int) {\n\
    \x20   for (i in n downTo 0) {\n\
    \x20       sink(i)\n\
    \x20   }\n\
    }\n\
    \n\
    fun elements(a: IntArray) {\n\
    \x20   for (x in a) {\n\
    \x20       sink(x)\n\
    \x20   }\n\
    }\n\
    \n\
    fun skipping(n: Int) {\n\
    \x20   for (i in 0..n) {\n\
    \x20       if (i == 2) continue\n\
    \x20       sink(i)\n\
    \x20   }\n\
    }\n\
    \n\
    fun nested(n: Int) {\n\
    \x20   for (i in 0..n) {\n\
    \x20       for (j in 0 until i) {\n\
    \x20           sink(j)\n\
    \x20       }\n\
    \x20   }\n\
    }\n\
    \n\
    fun oneLine(n: Int) {\n\
    \x20   for (i in 1..n) sink(i)\n\
    }\n";

#[test]
fn a_for_loop_update_carries_the_loop_line_like_kotlinc() {
    common::byte_diff_against_kotlinc("LoopLines", SOURCE, "store/LoopLinesKt")
        .expect("reference kotlinc is provisioned")
        .unwrap_or_else(|difference| panic!("{difference}"));
}
