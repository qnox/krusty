//! Use-site variance projections in type arguments — `Box<out T>` (covariant) and `Box<in T>`
//! (contravariant). Variance is JVM-erased, so the projection is parsed and dropped, keeping the bare
//! type. Previously `in` (a real keyword, unlike the soft `out`) was not skipped, so `Box<in T>` failed
//! to parse. Round-tripped on the JVM.

mod common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn in_and_out_projections() {
    const SRC: &str = "class Box<T>(val v: T)\n\
fun unwrap(b: Box<out Any>): Any = b.v\n\
fun put(b: Box<in String>): String = \"OK\"\n\
fun box(): String {\n\
    val b: Box<out String> = Box(\"OK\")\n\
    if (put(Box(\"x\")) != \"OK\") return \"f1\"\n\
    return unwrap(b) as String\n\
}\n";
    assert_eq!(run(SRC).expect("in/out projections parse + run"), "OK");
}

#[test]
fn nested_in_projection() {
    // `Box<in Box<String>>` — `in` before a nested generic.
    const SRC: &str = "class Box<T>(val v: T)\n\
fun f(b: Box<in Box<String>>): String = \"OK\"\n\
fun box(): String = f(Box(Box(\"x\")))\n";
    assert_eq!(run(SRC).expect("nested in projection"), "OK");
}
