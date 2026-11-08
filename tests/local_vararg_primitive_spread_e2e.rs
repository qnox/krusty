//! A primitive array spread passed to a local function's `vararg`. kotlinc measures the spread
//! against the whole `IntArray`, so the local function stays applicable and wins over the stdlib
//! `sum` overloads the same call could otherwise reach. The Wasm backends do not store primitive
//! arrays yet, so this program is checked against kotlinc on the JVM.
use super::common;

const SOURCE: &str = "fun ints(ys: IntArray): Int {\n\
    \x20   fun sum(vararg n: Int): Int = n[0] + n[n.size - 1]\n\
    \x20   fun at(vararg n: Int, index: Int = 1): Int = n[index] * 10\n\
    \x20   return sum(*ys) + sum(1, *ys) + at(*ys) + at(7, *ys, index = 2) + at(4, 5, index = 0)\n\
    }\n\
    fun box(): String = if (ints(intArrayOf(2, 3)) == 5 + 4 + 30 + 30 + 40) \"OK\" else \"Fail\"\n";

#[test]
fn a_primitive_array_spread_fills_a_local_vararg() {
    common::assert_class_matches_kotlinc(
        "LocalVarargPrimitiveSpread",
        SOURCE,
        "LocalVarargPrimitiveSpreadKt",
    );
    common::expect_box_same_as_kotlinc(SOURCE, "LocalVarargPrimitiveSpreadRun");
}
