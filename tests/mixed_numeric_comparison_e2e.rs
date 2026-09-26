//! Comparisons between different primitive number types, reachable through smart casts and through
//! mixed-type `compareTo` overloads, compare at the promoted type exactly as kotlinc emits them:
//! the narrower operand widens at runtime, and a constant operand widens at compile time, while
//! arithmetic keeps its constant's runtime widening.

use super::common;

const SOURCE: &str =
    "fun intEqualsLong(a: Any, b: Any): Boolean = a is Int && b is Long && a == b\n\
    fun byteEqualsShort(a: Any, b: Any): Boolean = a is Byte && b is Short && a == b\n\
    fun longEqualsConstant(a: Any): Boolean = a is Long && a == 3\n\
    fun constantAboveInt(a: Any): Boolean = a is Int && 7L > a\n\
    fun doubleBelowFloatConstant(d: Double): Boolean = d < 1.0F\n\
    fun intBelowLongConstant(i: Int): Boolean = i < 5L\n\
    fun intConstantBelowFloat(f: Float): Boolean = 2 < f\n\
    fun doublePlusFloatConstant(d: Double): Double = d + 1.0F\n\
    fun longBelowIntConstant(l: Long): Boolean = l < 5\n\
    fun floatBelowDoubleConstant(f: Float): Boolean = f < 2.5\n";

#[test]
fn mixed_numeric_comparisons_compile_like_kotlinc() {
    let pair = common::ModuleClassPair::compile(&[("Mixed.kt", SOURCE)], "MixedKt");
    for method in [
        "intEqualsLong",
        "byteEqualsShort",
        "longEqualsConstant",
        "constantAboveInt",
        "doubleBelowFloatConstant",
        "intBelowLongConstant",
        "intConstantBelowFloat",
        "doublePlusFloatConstant",
        "longBelowIntConstant",
        "floatBelowDoubleConstant",
    ] {
        let (kotlinc, krusty) = pair.method_code("MixedKt", method);
        assert_eq!(krusty, kotlinc, "{method}");
    }
}

/// `Int == Long` used to compare the `long` operand with `if_icmpne`, which the verifier rejects.
#[test]
fn a_smart_cast_int_equals_a_long_at_runtime() {
    let source = "fun equal(a: Any, b: Any): Boolean = a is Int && b is Long && a == b\n\
        fun box(): String {\n\
            if (!equal(3, 3L)) return \"fail equal\"\n\
            if (equal(3, 4L)) return \"fail different\"\n\
            return \"OK\"\n\
        }\n";
    let jdk = common::jdk_modules();
    assert_eq!(
        common::compile_and_run_box(
            source,
            "SmartCastEquality",
            &[common::stdlib_jar()],
            Some(jdk.as_path()),
        )
        .expect("krusty compiles and runs the fixture"),
        "OK"
    );
}
