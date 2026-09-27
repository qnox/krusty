//! A `super` call inside a value-class member runs on the boxed value: kotlinc's static `-impl`
//! boxes its carrier with `box-impl` and `invokespecial`s the supertype's member on the box. Boxing
//! the carrier as its primitive wrapper instead fails verification (`Integer` is not the class).

use super::common;

const SRC: &str = "interface Greeter {\n\
    \x20   fun greet(): String = \"K\"\n\
    }\n\
    @JvmInline value class Tag(val code: Long) : Greeter {\n\
    \x20   override fun greet(): String = \"O\" + super<Greeter>.greet()\n\
    }\n\
    @JvmInline value class Id(val raw: Int) {\n\
    \x20   fun identity(): Int = super.hashCode()\n\
    }\n\
    @JvmInline value class Name(val text: String) : Greeter {\n\
    \x20   override fun greet(): String = text + super<Greeter>.greet()\n\
    \x20   fun identity(): Int = super.hashCode()\n\
    }\n\
    @JvmInline value class MaybeName(val text: String?) : Greeter {\n\
    \x20   override fun greet(): String = super<Greeter>.greet()\n\
    \x20   fun identity(): Int = super.hashCode()\n\
    }\n\
    fun box(): String {\n\
    \x20   Id(1).identity()\n\
    \x20   Name(\"a\").identity()\n\
    \x20   MaybeName(null).identity()\n\
    \x20   val a: Greeter = Name(\"a\")\n\
    \x20   val b: Greeter = MaybeName(\"b\")\n\
    \x20   val c: Greeter = MaybeName(null)\n\
    \x20   if (a.greet() + b.greet() + c.greet() != \"aKKK\") return \"names\"\n\
    \x20   val greeter: Greeter = Tag(42L)\n\
    \x20   return greeter.greet()\n\
    }\n";

/// `member`'s instructions, call targets included, match kotlinc's.
fn assert_same_instructions(class: &str, member: &str) {
    let built = common::compare_with_kotlinc_plugin(
        "ValueClassSuperCall",
        SRC,
        class,
        &[common::stdlib_jar()],
        "25",
        &[],
    )
    .expect("reference kotlinc is provisioned");
    let reference = common::method_instructions(&built.reference, member);
    assert!(!reference.is_empty(), "kotlinc emits {class}.{member}");
    assert_eq!(
        common::method_instructions(&built.krusty, member),
        reference
    );
}

#[test]
fn an_interface_super_call_boxes_the_receiver() {
    assert_same_instructions("Tag", "greet-impl(long);");
}

#[test]
fn an_any_super_call_boxes_the_receiver() {
    assert_same_instructions("Id", "identity-impl(int);");
}

#[test]
fn a_reference_carrier_boxes_the_receiver() {
    assert_same_instructions("Name", "greet-impl(java.lang.String);");
    assert_same_instructions("Name", "identity-impl(java.lang.String);");
}

#[test]
fn a_nullable_reference_carrier_boxes_the_receiver() {
    assert_same_instructions("MaybeName", "greet-impl(java.lang.String);");
    assert_same_instructions("MaybeName", "identity-impl(java.lang.String);");
}

#[test]
fn super_calls_on_value_classes_run() {
    common::expect_box_ok_with_stdlib(SRC, "ValueClassSuperCall");
}
