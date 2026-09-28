//! A value class whose property is another value class over a reference carrier
//! (`ZN(val z: Z1?)` with `Z1(val x: En)`) stores that property as the inner carrier. `n.z!!` is
//! then already `Z1`'s carrier, and `.x` of it is the carrier itself: kotlinc reads it without a
//! cast. krusty took the read for a box and cast the enum to `Z1`.

use super::common;

const SRC: &str = "enum class En { N, A }\n\
    @JvmInline value class Z1(val x: En)\n\
    @JvmInline value class ZN(val z: Z1?)\n\
    fun wrap(x: En): ZN? = if (x.ordinal == 0) null else ZN(Z1(x))\n\
    fun read(n: ZN): En = n.z!!.x\n\
    fun box(): String {\n\
    \x20   if (wrap(En.N) != null) return \"fail null\"\n\
    \x20   return if (read(wrap(En.A)!!) == En.A) \"OK\" else \"fail read\"\n\
    }\n";

#[test]
fn a_nested_value_class_property_reads_as_its_carrier() {
    let built = common::compare_with_kotlinc_plugin(
        "NestedCarrier",
        SRC,
        "NestedCarrierKt",
        &[common::stdlib_jar()],
        "25",
        &[],
    )
    .expect("reference kotlinc is provisioned");
    let method = "public static final En read-uYmVlDY(En)";
    let reference = common::method_instructions(&built.reference, method);
    assert!(!reference.is_empty(), "kotlinc writes {method}");
    assert_eq!(
        common::method_instructions(&built.krusty, method),
        reference
    );
}

#[test]
fn a_nested_value_class_property_runs() {
    assert_eq!(common::expect_box_run_with_stdlib(SRC, "Main"), "OK");
}
