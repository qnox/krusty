//! A dependency's generic result specialized to a value class whose carrier is `Object`.
//!
//! `fun <T> pass(value: T): T` returns its argument through the erased `Object` slot, so a call
//! `pass(b)` with `b: Box` hands back the BOX, which the caller unboxes once. When `Box`'s own
//! carrier is also `Object` (`value class Box(val item: Any)`, or `Any?`), the unboxed result and
//! the generic slot share one physical type. The unbox must still happen exactly once, as kotlinc
//! emits it; a second `checkcast Box` over the carrier throws `ClassCastException`.

use super::common;

const LIB: &str = "package lib\n\
    @JvmInline value class Box(val item: Any)\n\
    @JvmInline value class NullableBox(val item: Any?)\n\
    fun makeBox(item: Any): Box = Box(item)\n\
    fun makeNullableBox(item: Any?): NullableBox = NullableBox(item)\n\
    fun <T> pass(value: T): T = value\n";

const SRC: &str = "import lib.*\n\
    fun relay(b: Box): Box = pass(b)\n\
    fun relayNullable(b: NullableBox): NullableBox = pass(b)\n\
    fun box(): String {\n\
    \x20   if (relay(makeBox(\"c\")).item != \"c\") return \"fail: Box\"\n\
    \x20   if (relayNullable(makeNullableBox(\"d\")).item != \"d\") return \"fail: NullableBox\"\n\
    \x20   return \"OK\"\n\
    }\n";

/// `name` is the source file's stem, so the functions live in the facade `{name}Kt`.
fn assert_same_method(name: &str, method: &str) {
    let facade = format!("{name}Kt");
    match common::method_code_diff_against_kotlinc(name, &[("Lib.kt", LIB)], SRC, &facade, method) {
        Some(Ok(())) => {}
        Some(Err(diff)) => panic!("{diff}"),
        None => panic!("{name}: reference toolchain unavailable"),
    }
}

#[test]
fn an_any_carried_generic_result_is_unboxed_once() {
    assert_same_method("Relay", "public static final java.lang.Object relay-");
}

#[test]
fn a_nullable_any_carried_generic_result_is_unboxed_once() {
    assert_same_method(
        "RelayNullable",
        "public static final java.lang.Object relayNullable-",
    );
}

#[test]
fn a_relayed_object_carried_value_class_keeps_its_value() {
    let lib =
        common::kotlinc_lib_out(&[("Lib.kt", LIB)]).expect("reference kotlinc is provisioned");
    let output = common::compile_and_run_box(SRC, "Main", &[lib, common::stdlib_jar()], None)
        .expect("krusty compiles and the JVM runs the box function");
    assert_eq!(output, "OK");
}
