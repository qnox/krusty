//! A `for`-loop variable may carry an explicit type — `for (i: Int in xs)`, including a widened
//! nullable type `for (c: Char? in str)`. The variable's value is the iterable's (non-null) element;
//! the annotation only widens, so it is accepted and discarded. Round-tripped on the JVM.

mod common;

fn run(src: &str) -> Option<String> {
    let jh = common::java_home()?;
    let sl = common::stdlib_jar()?;
    let jdk = std::path::PathBuf::from(format!("{jh}/lib/modules"));
    common::compile_and_run_box(src, "Main", &[sl], Some(&jdk))
}

#[test]
fn typed_int_loop_var_over_array() {
    const SRC: &str = "fun box(): String {\n\
    var s = 0\n\
    for (i: Int in intArrayOf(1, 2, 3)) { s += i }\n\
    return if (s == 6) \"OK\" else \"fail $s\"\n\
}\n";
    assert_eq!(run(SRC).expect("typed loop var"), "OK");
}

#[test]
fn typed_loop_var_over_range() {
    const SRC: &str = "fun box(): String {\n\
    var s = 0\n\
    for (i: Int in 1..3) { s += i }\n\
    return if (s == 6) \"OK\" else \"fail $s\"\n\
}\n";
    assert_eq!(run(SRC).expect("typed loop var over range"), "OK");
}

#[test]
fn nullable_typed_loop_var_downto() {
    const SRC: &str = "fun box(): String {\n\
    var s = 0\n\
    for (i: Int? in 4 downTo 1) { s += i!! }\n\
    return if (s == 10) \"OK\" else \"fail $s\"\n\
}\n";
    assert_eq!(run(SRC).expect("nullable typed loop var"), "OK");
}
