//! A lambda passed to an inline parameter that is not a function type is one object.
//!
//! Splicing the literal at every use makes `x === x` compare two instances, and
//! `inline/lambdaAsAny.kt` returns `FAIL`.
use super::common;

#[test]
fn inline_non_function_parameter_materializes_a_lambda_once() {
    const SRC: &str = "\
val sb = StringBuilder()\n\
inline fun foo(x: Any) {\n\
    sb.append(if (x === x) \"OK\" else \"FAIL\")\n\
}\n\
fun box(): String {\n\
    foo { 42 }\n\
    return sb.toString()\n\
}\n";
    common::expect_box_same_as_kotlinc(SRC, "LambdaAsAny");
}

#[test]
fn inline_type_parameter_materializes_a_lambda_once() {
    const SRC: &str = "\
inline fun <T> same(x: T): Boolean = x === x\n\
fun box(): String = if (same { 42 }) \"OK\" else \"FAIL\"\n";
    common::expect_box_same_as_kotlinc(SRC, "LambdaAsTypeParam");
}

#[test]
fn inline_function_parameter_still_splices_its_lambda() {
    const SRC: &str = "\
inline fun runIt(block: () -> String): String = block()\n\
fun box(): String = runIt { \"OK\" }\n";
    common::expect_box_same_as_kotlinc(SRC, "LambdaStillSpliced");
}

#[test]
fn inline_function_extension_receiver_is_invoked_as_a_value() {
    const SRC: &str = "\
inline fun (() -> String).runIt(): String = this()\n\
fun box(): String = { \"OK\" }.runIt()\n";
    common::expect_box_same_as_kotlinc(SRC, "LambdaExtensionValue");
}

/// A nullable function parameter is not an inline lambda slot. kotlinc accepts it only with
/// `noinline`, and the argument is one object.
#[test]
fn inline_nullable_function_parameter_materializes_its_lambda() {
    const SRC: &str = "\
inline fun same(noinline x: (() -> Int)?): Boolean = x === x\n\
fun box(): String = if (same { 1 }) \"OK\" else \"FAIL\"\n";
    common::expect_box_same_as_kotlinc(SRC, "NullableFunctionParameter");
}

/// A typealias of a non-null function type is still spliced, so the lambda may return from the
/// caller.
#[test]
fn inline_typealias_of_a_function_splices_its_lambda() {
    const SRC: &str = "\
typealias Block = () -> String\n\
inline fun runIt(block: Block): String {\n\
    block()\n\
    return \"FAIL\"\n\
}\n\
fun box(): String {\n\
    runIt { return \"OK\" }\n\
    return \"FAIL\"\n\
}\n";
    common::expect_box_same_as_kotlinc(SRC, "TypealiasFunctionSplice");
}
