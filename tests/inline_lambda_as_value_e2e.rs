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
    assert_eq!(
        common::compile_and_run_with_stdlib(SRC, "LambdaAsAny"),
        Some("OK".to_string())
    );
}

#[test]
fn inline_type_parameter_materializes_a_lambda_once() {
    const SRC: &str = "\
inline fun <T> same(x: T): Boolean = x === x\n\
fun box(): String = if (same { 42 }) \"OK\" else \"FAIL\"\n";
    assert_eq!(
        common::compile_and_run_with_stdlib(SRC, "LambdaAsTypeParam"),
        Some("OK".to_string())
    );
}

#[test]
fn inline_function_parameter_still_splices_its_lambda() {
    const SRC: &str = "\
inline fun runIt(block: () -> String): String = block()\n\
fun box(): String = runIt { \"OK\" }\n";
    assert_eq!(
        common::compile_and_run_with_stdlib(SRC, "LambdaStillSpliced"),
        Some("OK".to_string())
    );
}

#[test]
fn inline_function_extension_receiver_is_invoked_as_a_value() {
    const SRC: &str = "\
inline fun (() -> String).runIt(): String = this()\n\
fun box(): String = { \"OK\" }.runIt()\n";
    assert_eq!(
        common::compile_and_run_with_stdlib(SRC, "LambdaExtensionValue"),
        Some("OK".to_string())
    );
}
