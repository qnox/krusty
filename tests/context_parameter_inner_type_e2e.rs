//! A context parameter whose type is an inner class. Resolving `Outer.Inner` (or an unqualified
//! `Inner` inside a generic outer) consults the declaration's implicit receivers for the outer's
//! type arguments; the declaration's own context parameters are not in scope for their own types,
//! so that lookup must not ask for them again.

use super::common;

#[test]
fn a_context_parameter_of_an_inner_type_matches_kotlinc() {
    common::expect_box_same_as_kotlinc(
        "\
class Outer(val name: String) {
    inner class Inner {
        fun read() = name
    }
}

context(inner: Outer.Inner)
fun readInner(): String = inner.read()

fun box(): String = with(Outer(\"OK\").Inner()) { readInner() }
",
        "ContextInnerType",
    );
}

#[test]
fn a_context_parameter_of_an_unqualified_inner_type_in_a_generic_outer_matches_kotlinc() {
    common::expect_box_same_as_kotlinc(
        "\
class Holder<T>(val value: T) {
    inner class Item {
        fun get(): T = value
    }

    context(item: Item)
    fun read(): T = item.get()
}

fun box(): String {
    val holder = Holder(\"OK\")
    return with(holder.Item()) { holder.read() }
}
",
        "ContextGenericInnerType",
    );
}
