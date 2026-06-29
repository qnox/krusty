//! `x is T?` / `x !is T?` for a nullable PRIMITIVE target (`Int?`, `Long?`, `Boolean?`): `null` IS a
//! `T?`, a non-null value of the right runtime type is too. Lowers to `x == null || x instanceof <wrapper>`
//! (and the De Morgan dual for `!is`). Float/Double are excluded (a smart-cast could reach boxed IEEE
//! `==`). Round-tripped on the JVM.

mod common;

fn run(src: &str) -> Option<String> {
    let jh = common::java_home()?;
    let sl = common::stdlib_jar()?;
    let jdk = std::path::PathBuf::from(format!("{jh}/lib/modules"));
    common::compile_and_run_box(src, "Main", &[sl], Some(&jdk))
}

#[test]
fn is_nullable_int() {
    const SRC: &str = "fun box(): String {\n\
    val a: Any? = 5\n\
    val n: Any? = null\n\
    val s: Any? = \"x\"\n\
    if (a !is Int?) return \"f1\"\n\
    if (n !is Int?) return \"f2\"\n\
    if (s is Int?) return \"f3\"\n\
    if (n is Int) return \"f4\"\n\
    return \"OK\"\n\
}\n";
    assert_eq!(run(SRC).expect("is Int? compiles + runs"), "OK");
}

#[test]
fn is_nullable_long_and_boolean() {
    const SRC: &str = "fun box(): String {\n\
    val l: Any? = 7L\n\
    val b: Any? = true\n\
    val n: Any? = null\n\
    if (l !is Long?) return \"f1\"\n\
    if (b !is Boolean?) return \"f2\"\n\
    if (n !is Long?) return \"f3\"\n\
    if (l is Boolean?) return \"f4\"\n\
    return \"OK\"\n\
}\n";
    assert_eq!(run(SRC).expect("is Long?/Boolean? compiles + runs"), "OK");
}
