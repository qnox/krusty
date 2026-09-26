//! A primitive read from a Java generic (`list[0]` on a `java.util.ArrayList<Int>`, typed `Int!`)
//! is unboxed straight from the `Object` the call returns, as kotlinc does: unary operators consume
//! it at their primitive receiver, and the unbox goes through `Number` without an intermediate
//! `checkcast` to the wrapper.

use super::common;

const SOURCE: &str = "import java.util.ArrayList\n\
    fun negated(l: ArrayList<Int>): Int = -l[0]\n\
    fun unaryPlus(l: ArrayList<Int>): Int = +l[0]\n\
    fun inverted(l: ArrayList<Boolean>): Boolean = !l[0]\n\
    fun negatedDouble(l: ArrayList<Double>): Double = -l[0]\n\
    fun incremented(l: ArrayList<Int>): Int = l[0] + 1\n\
    fun explicitNegation(l: ArrayList<Long>): Long = l[0].unaryMinus()\n";

#[test]
fn platform_primitive_operands_compile_like_kotlinc() {
    let pair = common::ModuleClassPair::compile(&[("Platform.kt", SOURCE)], "PlatformKt");
    for method in [
        "negated",
        "unaryPlus",
        "inverted",
        "negatedDouble",
        "incremented",
        "explicitNegation",
    ] {
        let (kotlinc, krusty) = pair.method_code("PlatformKt", method);
        assert_eq!(krusty, kotlinc, "{method}");
    }
}

/// `-l[0]` used to negate the boxed `Integer` with `ineg`, which the verifier rejects.
#[test]
fn a_negated_java_list_element_runs() {
    let source = "import java.util.ArrayList\n\
        fun box(): String {\n\
            val l = ArrayList<Int>()\n\
            l.add(1)\n\
            val x = -l[0]\n\
            if (x != -1) return \"fail $x\"\n\
            return \"OK\"\n\
        }\n";
    let jdk = common::jdk_modules();
    assert_eq!(
        common::compile_and_run_box(
            source,
            "PlatformNegation",
            &[common::stdlib_jar()],
            Some(jdk.as_path()),
        )
        .expect("krusty compiles and runs the fixture"),
        "OK"
    );
}
