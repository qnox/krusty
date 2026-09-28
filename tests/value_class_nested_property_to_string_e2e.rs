//! A value class's sole property of another value class's type is that value class, carried as
//! its carrier. `o.toString()` on it calls the nested class's static `toString-impl` over the
//! carrier, as kotlinc does; krusty boxed the carrier as a `Long` and printed the bare number.

use super::common;

const SRC: &str = "@JvmInline value class Inner(val l: Long)\n\
    @JvmInline value class Outer(val o: Inner) {\n\
    \x20   fun show(): String = o.toString()\n\
    }\n";

#[test]
fn a_nested_value_class_property_is_printed_by_its_class() {
    let built = common::compare_with_kotlinc_plugin(
        "NestedPropertyToString",
        SRC,
        "Outer",
        &[common::stdlib_jar()],
        "25",
        &[],
    )
    .expect("reference kotlinc is provisioned");
    let method = "public static final java.lang.String show-impl(long)";
    let reference = common::method_instructions(&built.reference, method);
    assert!(!reference.is_empty(), "kotlinc writes show-impl");
    assert_eq!(
        common::method_instructions(&built.krusty, method),
        reference
    );
}

#[test]
fn a_nested_value_class_property_prints_its_class() {
    let src = format!(
        "{SRC}fun box(): String = if (Outer(Inner(6)).show() == \"Inner(l=6)\") \"OK\" else \"fail\"\n"
    );
    assert_eq!(common::expect_box_run_with_stdlib(&src, "Main"), "OK");
}
