//! Smart-cast within an `&&` condition: after `x is T` (or `x != null`) on the left, `x` is `T` while
//! evaluating the right operand (`x is String && x.length == 1`). Round-tripped under `-Xverify:all`.

mod common;

fn run(src: &str) -> Option<String> {
    let sl = common::stdlib_jar()?;
    let jdk = common::jdk_modules()?;
    common::compile_and_run_box(src, "C", &[sl], Some(&jdk))
}

#[test]
fn smartcast_in_and_condition() {
    const SRC: &str = "fun check(x: Any): Boolean = x is String && x.length == 2\n\
fun box(): String {\n\
    if (!check(\"ok\")) return \"fail string\"\n\
    if (check(\"too long\")) return \"fail len\"\n\
    if (check(42)) return \"fail nonstring\"\n\
    return \"OK\"\n\
}\n";
    let out = run(SRC).expect("smart-cast in && should compile + run");
    assert_eq!(out, "OK");
}

#[test]
fn smartcast_in_or_negated_condition() {
    // `x !is String || x.length` — reaching the `||` RHS means `x` IS a `String` (the LHS was false).
    const SRC: &str = "fun lenOk(x: Any): Boolean {\n\
    if (x !is String || x.length != 2) return false\n\
    return true\n\
}\n\
fun box(): String {\n\
    if (!lenOk(\"ok\")) return \"fail string\"\n\
    if (lenOk(\"too long\")) return \"fail len\"\n\
    if (lenOk(42)) return \"fail nonstring\"\n\
    return \"OK\"\n\
}\n";
    let out = run(SRC).expect("smart-cast in || (negated) should compile + run");
    assert_eq!(out, "OK");
}
