//! Smart-cast within an `&&` condition: after `x is T` (or `x != null`) on the left, `x` is `T` while
//! evaluating the right operand (`x is String && x.length == 1`). Round-tripped under `-Xverify:all`.

mod common;

fn run(src: &str) -> Option<String> {
    let jh = common::java_home()?;
    let sl = common::stdlib_jar()?;
    let jdk = std::path::PathBuf::from(format!("{jh}/lib/modules"));
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
