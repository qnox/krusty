//! `Unit?` is a nullable reference to `kotlin/Unit` (values `Unit.INSTANCE` or `null`), not a "primitive".
//! It is valid as a parameter, a local (a 1-slot reference, tracked in frames), and a closure result, and
//! compares with `null`. Round-tripped on the JVM.

mod common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn nullable_unit_param_and_local() {
    const SRC: &str = "fun isNull(x: Unit?) = x == null\n\
fun box(): String {\n\
    if (!isNull(null)) return \"fail 1\"\n\
    val x: Unit? = null\n\
    if (!isNull(x)) return \"fail 2\"\n\
    return \"OK\"\n\
}\n";
    assert_eq!(run(SRC).expect("Unit? param + local"), "OK");
}

#[test]
fn nullable_unit_closure_result() {
    const SRC: &str = "fun isNull(x: Unit?) = x == null\n\
fun box(): String {\n\
    val closure: () -> Unit? = { null }\n\
    return if (isNull(closure())) \"OK\" else \"fail\"\n\
}\n";
    assert_eq!(run(SRC).expect("Unit? closure result"), "OK");
}
