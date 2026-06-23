//! Unbound top-level function references `::foo` passed to a function-typed parameter. Lowered to the
//! same `invokedynamic` + `LambdaMetafactory` machinery as a lambda, with the impl method handle
//! pointing directly at the referenced function. Round-tripped against the JVM under `-Xverify:all`.

mod common;

#[test]
fn callable_refs_run() {
    let Some(java_home) = common::java_home() else {
        eprintln!("skipping callable_ref_e2e: set JAVA_HOME");
        return;
    };
    let Some(stdlib) = common::stdlib_jar() else {
        eprintln!("skipping callable_ref_e2e: no kotlin-stdlib jar found");
        return;
    };
    const SRC: &str = "fun inc(n: Int): Int = n + 1\n\
fun twice(n: Int): Int = n * 2\n\
fun apply1(f: (Int) -> Int, x: Int): Int = f(x)\n\
fun box(): String {\n\
if (apply1(::inc, 41) != 42) return \"f1\"\n\
if (apply1(::twice, 21) != 42) return \"f2\"\n\
return \"OK\"\n\
}\n";
    let jdk = std::path::PathBuf::from(format!("{java_home}/lib/modules"));
    let Some(out) = common::compile_and_run_box(SRC, "C", &[stdlib], Some(&jdk)) else {
        return;
    };
    assert_eq!(out, "OK");
}
