//! A call whose named arguments reorder it stores each argument in a temporary in source order,
//! except those kotlinc passes in place: a constant (folded ones included), a read of a `val`, a
//! parameter or `this`, a function literal, an unbound reference and an unbound class literal. A
//! scalar stored for a reference parameter is boxed where it is passed, not before it is stored. A
//! bound reference evaluates its receiver where it is written, so it is stored. Constructor
//! delegations follow the same rules.

use super::common;

const NAMED: &str = "object Marker
var counter = 0
val shared = 3
fun next(): Int { counter += 1; return counter }
fun take(first: Int, second: Any?): Int = first
class Holder(val label: String) {
    fun pass(): Int = take(second = this, first = next())
    fun member(): Int = label.length
}
fun make(): Holder { counter += 100; return Holder(\"m\") }
open class Base(val first: Int, val second: Any?) {
    constructor(label: String, first: Int) : this(second = next() + 1, first = first + label.length)
}
class Derived : Base(second = next() + 1, first = next())
class Delegating(flag: Boolean) : Base(if (flag) \"y\" else \"no\", next())
fun reordered(parameter: Int): Int {
    val fixed = \"v\"
    var changing = \"w\"
    changing += \"x\"
    var total = take(second = \"n\", first = next())
    total += take(second = fixed, first = next())
    total += take(second = changing, first = next())
    total += take(second = parameter, first = next())
    total += take(second = Marker, first = next())
    total += take(second = null, first = next())
    total += take(second = 1 + 2, first = next())
    total += take(second = \"x$fixed\", first = next())
    total += take(second = { parameter }, first = next())
    total += take(second = String::class, first = next())
    total += take(second = shared, first = next())
    total += take(second = 'c', first = next())
    total += take(second = make()::member, first = next())
    total += take(second = Holder::member, first = next())
    total += Derived().first + Delegating(true).first
    return total + Holder(\"h\").pass()
}
fun box(): String {
    val total = reordered(4)
    return if (total == 658 && counter == 119) \"OK\" else \"$total,$counter\"
}
";

#[test]
fn reordered_named_arguments_store_only_what_kotlinc_stores() {
    let sources = [("Named.kt", NAMED)];
    let holder = common::ModuleClassPair::compile(&sources, "Holder");
    assert!(
        holder.krusty == holder.kotlinc,
        "Holder differs from kotlinc"
    );
    let (kotlinc, krusty) =
        common::ModuleClassPair::compile(&sources, "NamedKt").method_code("NamedKt", "reordered");
    assert_eq!(krusty, kotlinc);
}

#[test]
fn reordered_named_arguments_evaluate_in_source_order() {
    common::expect_box_same_as_kotlinc(NAMED, "NamedOperands");
}
