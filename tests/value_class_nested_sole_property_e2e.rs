//! A value class over another one (`Outer(val inner: Inner)`, `Inner(val x: Any)`) shares its
//! carrier with it, so reading `outer.inner` is the carrier itself, as kotlinc reads it. krusty
//! took the read for the inner box and cast and unboxed it, which failed on the carrier. A
//! member returning `Outer` also hands a generic slot a non-null value, boxed without a null test.

use super::common;

const SRC: &str = "@JvmInline value class Inner(val x: Any)\n\
    @JvmInline value class Outer(val inner: Inner)\n\
    class Holder {\n\
    \x20   fun <T> pass(value: T): T = value\n\
    \x20   fun rewrap(outer: Outer): Outer = Outer(outer.inner)\n\
    \x20   fun make(): Outer = pass(rewrap(Outer(Inner(\"OK\"))))\n\
    \x20   fun read(): Any = make().inner.x\n\
    }\n\
    fun box(): String = Holder().read() as String\n";

fn compare(method: &str) {
    let built = common::compare_with_kotlinc_plugin(
        "NestedSoleProperty",
        SRC,
        "Holder",
        &[common::stdlib_jar()],
        "25",
        &[],
    )
    .expect("reference kotlinc is provisioned");
    let reference = common::method_instructions(&built.reference, method);
    assert!(!reference.is_empty(), "kotlinc writes {method}");
    assert_eq!(
        common::method_instructions(&built.krusty, method),
        reference
    );
}

#[test]
fn a_nested_value_class_property_is_the_shared_carrier() {
    compare("public final java.lang.Object rewrap-");
    compare("public final java.lang.Object read()");
}

#[test]
fn a_member_returning_a_value_class_is_boxed_without_a_null_test() {
    compare("public final java.lang.Object make-");
}

#[test]
fn a_nested_value_class_property_runs() {
    assert_eq!(common::expect_box_run_with_stdlib(SRC, "Main"), "OK");
}
