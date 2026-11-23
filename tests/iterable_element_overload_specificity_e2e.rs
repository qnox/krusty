//! A receiver that is itself an `Iterable<T>` makes both `minus(element: T)` (with `T` inferred as
//! the iterable) and `minus(elements: Iterable<T>)` applicable; the most specific candidate wins.

use super::common;

#[test]
fn an_iterable_element_type_prefers_the_iterable_overload() {
    const SRC: &str = "class Item(val n: Int) : Iterable<Item> {\n\
    override fun iterator(): Iterator<Item> = listOf(this).iterator()\n\
}\n\
fun f(a: List<Item>, b: List<Item>): List<Item> { val c = a - b.toSet(); return c }\n\
fun box(): String {\n\
    val x = Item(1)\n\
    val y = Item(2)\n\
    return if (f(listOf(x, y), listOf(x)).size == 1) \"OK\" else \"fail\"\n\
}\n";
    let result = common::compiler_diagnostics(&[("Main.kt", SRC)], &[]);
    assert_eq!(
        (result.reference_code, result.reference_stderr.as_str()),
        (0, ""),
        "kotlinc rejected the fixture"
    );
    assert_eq!(
        (result.krusty_code, result.krusty_stderr.as_str()),
        (0, ""),
        "krusty rejected the fixture"
    );
    assert_eq!(common::kotlinc_box_result(SRC), "OK");
    common::expect_box_ok_with_stdlib(SRC, "IterableElementOverloadSpecificity");
}
