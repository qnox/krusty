//! A captured `var` lives in a `Ref$XxxRef` holder. kotlinc leaves the holder's `element` unset when
//! the initializer is a constant equal to the field's default: zero (positive zero for a
//! floating-point value), `false`, the `'\u0000'` char, or `null` in an object holder. It stores
//! every other initializer, including `-0.0`, an unsigned zero and a boxed zero.

use super::common;

const CELLS: &str = "var total = 0L
fun record(value: Any?) { if (value != null) total += 1 }
fun run(f: () -> Unit) = f()
fun cells() {
    var a = 0; var b = false; var c = 0L; var d: String? = null; var e = 5; var f = 0.0
    var g = 'a'; var h = '\\u0000'; var i: Any? = \"x\"; var j = 0.0f; var k: Byte = 0
    var m = -0.0; var n: Int? = 0; var u = 0u; var w: Short = 0; var o: Int? = null; var p = 0 + 0
    run {
        a++; b = true; c++; d = \"d\"; e++; f += 1; g = 'b'; h = 'c'; i = null; j += 1; k++
        m += 1; n = 1; u++; w++; o = 2; p++
        record(a); record(b); record(c); record(d); record(e); record(f); record(g); record(h)
        record(i); record(j); record(k); record(m); record(n); record(u); record(w); record(o); record(p)
    }
}
fun box(): String {
    cells()
    return if (total == 16L) \"OK\" else \"fail\"
}
";

#[test]
fn a_shared_cell_leaves_a_default_initializer_unstored() {
    let pair = common::ModuleClassPair::compile(&[("Cells.kt", CELLS)], "CellsKt");
    let (kotlinc, krusty) = pair.method_code("CellsKt", "cells");
    assert_eq!(krusty, kotlinc);
}

#[test]
fn a_shared_cell_with_an_unstored_initializer_runs_like_kotlinc() {
    common::expect_box_same_as_kotlinc(CELLS, "Cells");
}
