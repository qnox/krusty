//! Scope functions with a receiver-function value argument (`x.apply(block)`), not only literal
//! receiver lambdas (`x.apply { ... }`). The stdlib `apply` body is private `@InlineOnly`, so resolution
//! must accept the function value and the backend must splice the real body.

mod common;

#[test]
fn apply_accepts_receiver_function_value_argument() {
    let Some(java_home) = common::java_home() else {
        eprintln!("skipping scope_function_value_arg_e2e: set JAVA_HOME");
        return;
    };
    let Some(stdlib) = common::stdlib_jar() else {
        eprintln!("skipping scope_function_value_arg_e2e: no kotlin-stdlib jar found");
        return;
    };
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
    let jdk = std::path::PathBuf::from(format!("{java_home}/lib/modules"));
    let Some(out) = common::compile_and_run_box(SRC, "ScopeValueArg", &[stdlib], Some(&jdk)) else {
        return;
    };
    assert_eq!(out, "OK");
}
