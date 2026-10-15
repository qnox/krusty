//! An `@InlineOnly` call's receiver read through a field, a getter or `!!` stays in place.
//!
//! kotlinc moves each argument's code to where the inlined body loads it whenever that code stores
//! no local and jumps nowhere outside itself (`InplaceArgumentsMethodTransformer`). A field read, a
//! property getter's invoke and a `!!` check qualify, so `h.items += F("x")` loads the receiver,
//! builds the element and calls `add` with no temporary.

use super::common;

const SOURCE: &str = "class F(val m: String)\n\
    class H {\n\
    \x20 val items: MutableList<F> = ArrayList()\n\
    \x20 fun own() { items += F(\"own\") }\n\
    }\n\
    val top: MutableList<String> = ArrayList()\n\
    fun outside(h: H) { h.items += F(\"outside\") }\n\
    fun asserted(h: H?) { h!!.items += F(\"asserted\") }\n\
    fun statics() { top += \"top\"; top -= \"gone\" }\n\
    fun box(): String {\n\
    \x20 val h = H()\n\
    \x20 h.own(); outside(h); asserted(h); statics()\n\
    \x20 val seen = h.items.joinToString { it.m } + top\n\
    \x20 return if (seen == \"own, outside, asserted[top]\") \"OK\" else seen\n\
    }\n";

#[test]
fn property_receivers_of_an_inline_only_call_stay_in_place_as_kotlinc_does() {
    let classes = common::classes_against_kotlinc_module(&[("Receivers.kt", SOURCE)]);
    assert_eq!(classes.differences(), Vec::<String>::new());
}

#[test]
fn property_receivers_of_an_inline_only_call_run() {
    common::expect_box_ok_with_stdlib(SOURCE, "in-place property receivers");
}
