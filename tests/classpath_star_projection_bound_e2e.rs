//! A star projection of a classpath class whose type parameter is bounded (`Discounted<*>` over
//! `class Discounted<P : Product>`) denotes the same type wherever it is written.
//!
//! The readable upper bound of `*` is the parameter's declared bound (`out Product`). Compact
//! signature resolution read that bound only from classes of the module being compiled, so a
//! declaration header spelled `Discounted<*>` as `Discounted<out Any?>` while the body checker, which
//! reads the class through the classpath provider, spelled it `Discounted<out Product>`. The two
//! "equal" types then disagreed: a smart cast to `Discounted<*>` no longer matched a `Discounted<*>`
//! parameter, so no cast reached the call (a `VerifyError`), and an override taking `Discounted<*>`
//! no longer matched the interface member it overrides, so its bridge was missing
//! (`AbstractMethodError`).
use super::common;

const LIB: &str = "package lib\n\
sealed class Product(val price: Long) {\n\
    class Simple(price: Long) : Product(price)\n\
    class Bundle(val parts: List<Product>) : Product(parts.sumOf { it.price })\n\
}\n\
sealed class Line<out P : Product>(val product: P, val qty: Int) {\n\
    class Standard(product: Product.Simple, qty: Int) : Line<Product.Simple>(product, qty)\n\
    class Discounted<P : Product>(product: P, qty: Int, val percent: Int) : Line<P>(product, qty)\n\
}\n";

const VISITOR: &str = "import lib.Line\n\
import lib.Product\n\
interface Visitor<out R> {\n\
    fun standard(line: Line.Standard): R\n\
    fun discounted(line: Line.Discounted<*>): R\n\
}\n\
fun lines(): List<Line<*>> {\n\
    val simple = Product.Simple(10)\n\
    return listOf(Line.Standard(simple, 1), Line.Discounted(Product.Bundle(listOf(simple)), 2, 5))\n\
}\n";

/// The smart-cast receiver reaches `discounted` through a `checkcast`.
#[test]
fn a_smart_cast_to_a_classpath_star_projection_is_passed_with_its_cast() {
    let main = format!(
        "{VISITOR}\
fun <R> Line<*>.accept(v: Visitor<R>): R = when (this) {{\n\
    is Line.Standard -> v.standard(this)\n\
    is Line.Discounted<*> -> v.discounted(this)\n\
}}\n\
fun box(): String {{\n\
    val counter = object : Visitor<Int> {{\n\
        override fun standard(line: Line.Standard) = line.qty\n\
        override fun discounted(line: Line.Discounted<*>) = line.qty * 2 + line.percent\n\
    }}\n\
    val total = lines().sumOf {{ it.accept(counter) }}\n\
    return if (total == 10) \"OK\" else \"fail: $total\"\n\
}}\n"
    );
    let Some(result) = common::expect_box_run_against("StarBoundSmartCast", LIB, &main) else {
        eprintln!("skip: toolchain unavailable");
        return;
    };
    assert_eq!(result, "OK");
}

/// The local anonymous visitor's `discounted` overrides the interface member, so a call through
/// `Visitor<R>` reaches it through the `Object`-returning bridge.
#[test]
fn an_override_taking_a_classpath_star_projection_gets_its_bridge() {
    let main = format!(
        "{VISITOR}\
fun <R> Line<*>.accept(v: Visitor<R>): R = when (this) {{\n\
    is Line.Standard -> v.standard(this)\n\
    is Line.Discounted<*> -> v.discounted(this as Line.Discounted<*>)\n\
}}\n\
fun box(): String {{\n\
    val counter = object : Visitor<Int> {{\n\
        override fun standard(line: Line.Standard) = line.qty\n\
        override fun discounted(line: Line.Discounted<*>) = line.qty * 2 + line.percent\n\
    }}\n\
    val total = lines().sumOf {{ it.accept(counter) }}\n\
    return if (total == 10) \"OK\" else \"fail: $total\"\n\
}}\n"
    );
    let Some(result) = common::expect_box_run_against("StarBoundBridge", LIB, &main) else {
        eprintln!("skip: toolchain unavailable");
        return;
    };
    assert_eq!(result, "OK");
}
