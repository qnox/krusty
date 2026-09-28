//! A reference to a value class's constructor calls its static `constructor-impl`, which returns
//! the carrier. kotlinc reflects the reference as `constructor-impl(I)I` and boxes what `invoke`
//! returns with `box-impl`; krusty reflected `<init>(I)V` and boxed the carrier as an `Integer`,
//! which a caller then failed to cast to the value class.

use super::common;

const SRC: &str = "@JvmInline value class Z(val x: Int)\n\
    fun mk(): (Int) -> Z = ::Z\n";

#[test]
fn a_value_class_constructor_reference_matches_kotlinc() {
    let built = common::compare_with_kotlinc_plugin(
        "ValueClassConstructorReference",
        SRC,
        "ValueClassConstructorReferenceKt$mk$1",
        &[common::stdlib_jar()],
        "25",
        &[],
    )
    .expect("reference kotlinc is provisioned");
    for method in [
        "ValueClassConstructorReferenceKt$mk$1();",
        "public java.lang.Object invoke(java.lang.Object);",
    ] {
        let reference = common::method_instructions(&built.reference, method);
        assert!(!reference.is_empty(), "kotlinc writes {method}");
        assert_eq!(
            common::method_instructions(&built.krusty, method),
            reference,
            "{method}"
        );
    }
}

#[test]
fn a_value_class_constructor_reference_returns_the_box() {
    let src = format!("{SRC}fun box(): String = if (mk()(42).x == 42) \"OK\" else \"fail\"\n");
    assert_eq!(common::expect_box_run_with_stdlib(&src, "Main"), "OK");
}
