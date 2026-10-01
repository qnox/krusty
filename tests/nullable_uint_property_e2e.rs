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
