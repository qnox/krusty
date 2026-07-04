//! Scope functions with a receiver-function value argument (`x.apply(block)`), not only literal
//! receiver lambdas (`x.apply { ... }`). The stdlib `apply` body is private `@InlineOnly`, so resolution
//! must accept the function value and the backend must splice the real body.

use super::common;

#[test]
fn apply_accepts_receiver_function_value_argument() {
    const SRC: &str = "// WITH_STDLIB\n\
class Buildee<T> {\n\
    var out: String = \"\"\n\
    fun yield(arg: T) { out = arg.toString() }\n\
}\n\
fun <T> build(instructions: Buildee<T>.() -> Unit): Buildee<T> {\n\
    return Buildee<T>().apply(instructions)\n\
}\n\
fun box(): String {\n\
    val b = build<String> { yield(\"OK\") }\n\
    return b.out\n\
}\n";
    common::assert_box_ok_with_stdlib(SRC, "ScopeValueArg");
}
