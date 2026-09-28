//! `W?` for a value class over `Any?` is the box, since the carrier's own null cannot tell it apart
//! from a null `W`. Passing such a `W?` where `Any?` is expected keeps the box. The coercion has
//! the carrier's type, which krusty took for a read of the class's property and unboxed, failing
//! on a null `W?`.

use super::common;

const SRC: &str = "@JvmInline value class Wrapped(val a: Any?)\n\
    var result: Any? = null\n\
    fun keep(vc: Wrapped?) { result = vc }\n\
    fun box(): String {\n\
    \x20   keep(null)\n\
    \x20   if (result != null) return \"fail null\"\n\
    \x20   keep(Wrapped(\"O\"))\n\
    \x20   return if (result == Wrapped(\"O\")) \"OK\" else \"fail value\"\n\
    }\n";

#[test]
fn a_boxed_nullable_value_class_passed_as_any_keeps_its_box() {
    let built = common::compare_with_kotlinc_plugin(
        "NullableAnyCarrier",
        SRC,
        "NullableAnyCarrierKt",
        &[common::stdlib_jar()],
        "25",
        &[],
    )
    .expect("reference kotlinc is provisioned");
    let method = "public static final void keep-";
    let reference = common::method_instructions(&built.reference, method);
    assert!(!reference.is_empty(), "kotlinc writes {method}");
    assert_eq!(
        common::method_instructions(&built.krusty, method),
        reference
    );
}

#[test]
fn a_boxed_nullable_value_class_passed_as_any_runs() {
    common::expect_box_same_as_kotlinc(SRC, "ValueClassNullableAnyCarrier");
}
