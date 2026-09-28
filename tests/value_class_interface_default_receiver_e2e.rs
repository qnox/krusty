//! A value class calling a default-argument member it inherits from an interface passes its box
//! to the interface's static `$default` stub, whose receiver is the interface, as kotlinc does.
//! krusty boxed the `Long` carrier with `Long.valueOf`, which the stub's interface call rejects.

use super::common;

const SRC: &str = "interface Sum {\n\
    \x20   fun f(a: Long = 1L, b: Long = 2L): Long\n\
    }\n\
    @JvmInline value class Base(val x: Long) : Sum {\n\
    \x20   override fun f(a: Long, b: Long) = a + b + x\n\
    }\n\
    fun call(): Long = Base(2L).f()\n\
    fun box(): String = if (call() == 5L) \"OK\" else \"fail\"\n";

#[test]
fn an_inherited_default_call_boxes_its_value_class_receiver() {
    let built = common::compare_with_kotlinc_plugin(
        "InterfaceDefaultReceiver",
        SRC,
        "InterfaceDefaultReceiverKt",
        &[common::stdlib_jar()],
        "25",
        &[],
    )
    .expect("reference kotlinc is provisioned");
    let method = "public static final long call()";
    let reference = common::method_instructions(&built.reference, method);
    assert!(!reference.is_empty(), "kotlinc writes {method}");
    assert_eq!(
        common::method_instructions(&built.krusty, method),
        reference
    );
}

#[test]
fn an_inherited_default_call_on_a_value_class_runs() {
    assert_eq!(common::expect_box_run_with_stdlib(SRC, "Main"), "OK");
}
