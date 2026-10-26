//! A vararg argument written over several lines. kotlinc lowers the vararg to `tmp = new T[n];
//! tmp[i] = e_i; tmp` (or a spread builder's `add`/`addSpread` calls) at the vararg's offsets,
//! which start at its first element. Its line therefore returns before each element store, each
//! builder call and the final array load. An array-factory call (`intArrayOf(...)`) becomes its
//! vararg, and an array literal spread into a vararg joins the enclosing array.

use super::common;

const PACKED: &str = r#"fun f(x: Int): String = x.toString()
fun n(x: Int): Int = x + 1
fun g(vararg xs: String): Int = xs.size
class Box(vararg val items: Int)

fun box(): String {
    val listed = listOf(
        f(1),
        "two",
        f(3),
    )
    val counted = g(
        f(4),
        "five",
    )
    val ints = intArrayOf(
        n(1),
        n(2),
    )
    val built = Box(
        n(3),
        4,
    )
    val named = g(
        xs = arrayOf(
            f(8),
        ),
    )
    val local = run {
        fun h(vararg ys: Int) = ys.sum()
        h(
            n(5),
            6,
        )
    }
    val result = "${listed.size}$counted${ints.sum()}${built.items.size}$named$local"
    return if (result == "3252112") "OK" else result
}
"#;

const SPREAD: &str = r#"fun f(x: Int): String = x.toString()
fun g(vararg xs: String): Int = xs.size
fun gi(vararg xs: Int): Int = xs.sum()

fun box(): String {
    val arr = arrayOf("p", "q")
    val ints = intArrayOf(1, 2)
    val spread = g(
        *arr,
        f(7),
    )
    val primitive = gi(
        *ints,
        3,
    )
    val flattened = g(
        *arrayOf(
            "y",
            f(9),
        ),
        "w",
    )
    val sole = g(*arrayOf("a", "b"))
    val inline = g(*arr, "x")
    val result = "$spread$primitive$flattened$sole$inline"
    return if (result == "36323") "OK" else result
}
"#;

#[test]
fn multiline_varargs_and_array_factories_match_kotlinc_classes() {
    common::assert_classes_identical_to_kotlinc("VarargLines", PACKED, &["VarargLinesKt", "Box"]);
    common::expect_box_same_as_kotlinc(PACKED, "VarargLines");
}

#[test]
fn multiline_spreads_and_spread_array_literals_match_kotlinc_classes() {
    common::assert_classes_identical_to_kotlinc("SpreadLines", SPREAD, &["SpreadLinesKt"]);
    common::expect_box_same_as_kotlinc(SPREAD, "SpreadLines");
}
