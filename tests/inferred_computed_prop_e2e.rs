//! A computed property with an INFERRED type (`val xx get() = x`, no explicit annotation) lowers — the
//! type comes from the getter body. Covers both a plain class and a value-class member. Round-tripped
//! under `-Xverify:all`.

mod common;

fn run(src: &str) -> Option<String> {
    let jh = common::java_home()?;
    let sl = common::stdlib_jar()?;
    let jdk = std::path::PathBuf::from(format!("{jh}/lib/modules"));
    common::compile_and_run_box(src, "C", &[sl], Some(&jdk))
}

#[test]
fn inferred_computed_member() {
    const SRC: &str = "class C(val x: Int) { val xx get() = x }\n\
fun box(): String = if (C(42).xx == 42) \"OK\" else \"fail\"\n";
    assert_eq!(
        run(SRC).expect("inferred computed prop compiles + runs"),
        "OK"
    );
}

#[test]
fn inferred_computed_member_in_value_class() {
    const SRC: &str = "@JvmInline\n\
value class Z(val x: Int) { val xx get() = x }\n\
fun box(): String = if (Z(42).xx == 42) \"OK\" else \"fail\"\n";
    assert_eq!(
        run(SRC).expect("value-class computed member compiles + runs"),
        "OK"
    );
}
