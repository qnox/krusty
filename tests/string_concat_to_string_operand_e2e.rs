//! A concatenation appends a `toString()` call's receiver itself, as kotlinc's
//! `FlattenStringConcatenationLowering` does: `Any.toString`, an override of it in this module, an
//! inherited override and a data class's generated one. A `super.toString()` call, and a value
//! class's `toString()` (a static call by then), keep their call.
use super::common;

const SOURCE: &str = "package store\n\
                      \n\
                      class Plain(val n: Int)\n\
                      class Custom(val n: Int) {\n\
                      \x20   override fun toString(): String = \"C\" + n\n\
                      }\n\
                      data class Record(val n: Int)\n\
                      open class Base {\n\
                      \x20   override fun toString(): String = \"B\"\n\
                      }\n\
                      class Derived : Base() {\n\
                      \x20   fun show(): String = \"d \" + this.toString() + super.toString()\n\
                      }\n\
                      interface Shape\n\
                      @JvmInline value class Wrap(val n: Int)\n\
                      \n\
                      fun plain(p: Plain): String = \"p \" + p.toString()\n\
                      fun custom(c: Custom): String = \"c \" + c.toString()\n\
                      fun record(r: Record): String = \"r ${r.toString()}\"\n\
                      fun text(s: String): String = \"s \" + s.toString()\n\
                      fun shape(s: Shape): String = \"sh \" + s.toString()\n\
                      fun derived(d: Derived): String = \"dv \" + d.toString()\n\
                      fun nested(a: String, b: Int): String = \"x \" + (a + b).toString() + \"!\"\n\
                      fun wrap(w: Wrap): String = \"w \" + w.toString()\n";

fn assert_identical(class: &str) {
    common::byte_diff_against_kotlinc_cp(
        "ToStringConcatenationOperand",
        SOURCE,
        class,
        &[common::stdlib_jar()],
    )
    .expect("reference kotlinc is provisioned")
    .unwrap_or_else(|diff| panic!("{class} differs from kotlinc: {diff}"));
}

#[test]
fn to_string_operands_are_appended_by_receiver_like_kotlinc() {
    assert_identical("store/ToStringConcatenationOperandKt");
}

#[test]
fn a_super_to_string_operand_keeps_its_call_like_kotlinc() {
    assert_identical("store/Derived");
}
