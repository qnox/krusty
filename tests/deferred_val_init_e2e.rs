//! A local `val x: T` may be declared without an initializer and assigned later (deferred
//! initialization, e.g. one assignment per `if`/`when` branch, or a branch that diverges). krusty treats
//! it like a once-assigned `var`: a synthetic default is written at the declaration and overwritten
//! before any read. (A nullable `val?` is excluded — it needs smart-cast-after-assignment.) Round-tripped.

mod common;

fn run(src: &str) -> Option<String> {
    let jh = common::java_home()?;
    let sl = common::stdlib_jar()?;
    let jdk = std::path::PathBuf::from(format!("{jh}/lib/modules"));
    common::compile_and_run_box(src, "Main", &[sl], Some(&jdk))
}

#[test]
fn deferred_val_assigned_in_branches() {
    const SRC: &str = "fun box(): String {\n\
    val x: Int\n\
    if (true) x = 5 else x = 6\n\
    return if (x == 5) \"OK\" else \"fail $x\"\n\
}\n";
    assert_eq!(run(SRC).expect("deferred val"), "OK");
}

#[test]
fn deferred_val_reference_type() {
    const SRC: &str = "fun box(): String {\n\
    val s: String\n\
    val n = 3\n\
    if (n > 2) s = \"O\" else s = \"X\"\n\
    return s + \"K\"\n\
}\n";
    assert_eq!(run(SRC).expect("deferred val reference"), "OK");
}

#[test]
fn deferred_val_with_diverging_branch() {
    const SRC: &str = "fun fail(): Nothing = throw RuntimeException(\"x\")\n\
fun box(): String {\n\
    val a: String\n\
    if (true) { a = \"OK\" } else { fail() }\n\
    return a\n\
}\n";
    assert_eq!(run(SRC).expect("deferred val with diverging branch"), "OK");
}
