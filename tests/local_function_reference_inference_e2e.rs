//! A local function reference passed where the enclosing call still infers its type arguments
//! types as the local function's own shape, so the call's result is the function's return type.

use super::common::expect_box_same_as_kotlinc;

#[test]
fn a_local_function_reference_fixes_the_type_arguments_of_the_enclosing_call() {
    expect_box_same_as_kotlinc(
        r#"
fun <T, R> apply1(t: T, f: (T) -> R): R = f(t)

fun box(): String {
    fun rewrite(line: String): String = line + "K"
    fun size(line: String): Int = line.length
    val let = "O".let(::rewrite)
    val applied = apply1("O", ::rewrite)
    val length = apply1("abc", ::size) + 1
    return if (let.length == 2 && applied.length == 2 && length == 4) let else "$let $applied $length"
}
"#,
        "LocalFunctionReferenceInference",
    );
}
