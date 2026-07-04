//! Short-form bracket destructuring in a lambda parameter: `{ [a, b] -> … }` (NameBasedDestructuring).
//! Identical to the parenthesized `{ (a, b) -> … }` form — binds one synthetic parameter and prepends
//! `val [a, b] = it`. Round-tripped on the JVM.

mod common;

fn run(src: &str) -> Option<String> {
    let sl = common::stdlib_jar()?;
    let jdk = common::jdk_modules()?;
    common::compile_and_run_box(src, "Main", &[sl], Some(&jdk))
}

#[test]
fn bracket_destructured_lambda_param() {
    const SRC: &str = "// LANGUAGE: +NameBasedDestructuring\n\
fun box(): String {\n\
    val p = \"O\" to \"K\"\n\
    return listOf(p).map { [a, b] -> a + b }.first()\n\
}\n";
    assert_eq!(run(SRC).expect("bracket lambda param"), "OK");
}

#[test]
fn bracket_lambda_param_with_underscore() {
    const SRC: &str = "// LANGUAGE: +NameBasedDestructuring\n\
fun box(): String {\n\
    val p = \"OK\" to 0\n\
    return listOf(p).map { [a, _] -> a }.first()\n\
}\n";
    assert_eq!(run(SRC).expect("bracket lambda param with _"), "OK");
}
