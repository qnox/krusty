//! A value class whose sole property is a NULLABLE value class stored unboxed renders that
//! property through the nested class's static `toString-impl`, behind a null test, as kotlinc does:
//! `N(raw=ULongArray(storage=[2]))` and `V(s=S(v=a))`, not the bare carrier's identity
//! (`N(raw=[J@1bb344e0)`) or its text (`V(s=a)`). A null one renders `null`.
//!
//! The unboxed storage is the representation fact that decides it: `S?` over a non-null `String`
//! and an unsigned array's `UIntArray?` are their carrier, so the class must be named; `I?` over
//! `Int` and `UInt?` are the boxed class, whose own `toString` already names it, and stay appended
//! as they are.

use super::common;

const SRC: &str = "@file:OptIn(ExperimentalUnsignedTypes::class)\n\
    @JvmInline value class S(val v: String)\n\
    @JvmInline value class V(val s: S?)\n\
    @JvmInline value class NU(val raw: UIntArray?)\n\
    @JvmInline value class NL(val raw: ULongArray?)\n\
    @JvmInline value class I(val x: Int)\n\
    @JvmInline value class NI(val raw: I?)\n\
    @JvmInline value class NUI(val raw: UInt?)\n";

#[test]
fn a_nullable_unboxed_value_class_property_renders_like_kotlinc() {
    // PLATFORM-SPECIFIC: the whole JVM classfiles, through both string-concatenation strategies.
    for (class, marker) in [
        ("V", "java.lang.String toString-impl(java.lang.String)"),
        ("NU", "java.lang.String toString-impl(int[])"),
        ("NL", "java.lang.String toString-impl(long[])"),
        ("NI", "java.lang.String toString-impl(I)"),
        ("NUI", "java.lang.String toString-impl(kotlin.UInt)"),
    ] {
        for target in ["1.8", "25"] {
            let built = common::compare_with_kotlinc_plugin(
                "NullableNestedToString",
                SRC,
                class,
                &[common::stdlib_jar()],
                target,
                &[],
            )
            .expect("reference kotlinc is provisioned");
            let reference = common::method_instructions(&built.reference, marker);
            assert!(
                !reference.is_empty(),
                "{target} {class}: kotlinc writes {marker}"
            );
            assert_eq!(
                common::method_instructions(&built.krusty, marker),
                reference,
                "{target} {class}"
            );
        }
    }
    // `V`, `NU` and `NL` are compared by their `toString-impl` above: their `equals-impl` over
    // the nullable carrier is not yet kotlinc's null-guarded comparison.
    common::assert_classes_identical_to_kotlinc(
        "NullableNestedClasses",
        SRC,
        &["S", "I", "NI", "NUI"],
    );
}

#[test]
fn a_nullable_unboxed_value_class_property_prints_like_kotlinc() {
    let src = include_str!("box_regressions/valueClassNullableNestedToString.kt");
    let source =
        krusty::conformance::prepare_test_source(src, krusty::conformance::TestTarget::Jvm);
    common::expect_box_same_as_kotlinc(&source, "value_class_nullable_nested_to_string");
}
