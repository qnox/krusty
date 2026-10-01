//! A nullable unsigned property is already the box its getter returns.
//!
//! `d.x?.let { d.x.toString() }` must null-check that box and unbox it once for
//! `toString`. Boxing the getter result again calls `UInt.box-impl` on a reference.
use super::common::expect_box_same_as_kotlinc;

#[test]
fn nullable_uint_property_safe_call_matches_kotlinc() {
    expect_box_same_as_kotlinc(
        "class D(val x: UInt?)\n\
         class E(val x: Any)\n\
         fun f(d: D): String = d.x?.let { d.x.toString() } ?: \"\"\n\
         fun g(e: E): String {\n\
         \x20 if (e.x is UInt) return e.x.toString()\n\
         \x20 return \"\"\n\
         }\n\
         fun box(): String {\n\
         \x20 if (f(D(42u)) != \"42\") return \"f=${f(D(42u))}\"\n\
         \x20 if (f(D(null)) != \"\") return \"null\"\n\
         \x20 if (g(E(42u)) != \"42\") return \"g=${g(E(42u))}\"\n\
         \x20 if (g(E(\"no\")) != \"\") return \"other\"\n\
         \x20 return \"OK\"\n\
         }\n",
        "nullable_uint_property_safe_call",
    );
}

/// A constructor argument that cannot stay on the stack is spilled and reloaded at the
/// descriptor's operand type. `Array<UInt>` is `[Lkotlin/UInt;`, not the primitive `[I`.
#[test]
fn spilled_unsigned_constructor_arguments_match_kotlinc() {
    expect_box_same_as_kotlinc(
        "class Holder(val xs: Array<UInt>, val n: UInt?)\n\
         fun box(): String {\n\
         \x20 val xs = arrayOf(7u)\n\
         \x20 val held = Holder(try { xs } finally { }, try { 7u } finally { })\n\
         \x20 val first = held.xs[0]\n\
         \x20 val number = held.n\n\
         \x20 return if (first == 7u && number == 7u) \"OK\" else \"xs=$first n=$number\"\n\
         }\n",
        "spilled_unsigned_constructor_arguments",
    );
}
