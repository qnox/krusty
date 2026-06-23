//! Non-capturing lambdas `{ a -> … }` passed to a function-typed parameter, lowered to
//! `invokedynamic` + `LambdaMetafactory` producing a `kotlin/jvm/functions/FunctionN`, then invoked
//! through `FunctionN.invoke`. Round-tripped against the JVM under `-Xverify:all`.

mod common;

#[test]
fn lambdas_run() {
    let Some(java_home) = common::java_home() else {
        eprintln!("skipping lambda_e2e: set JAVA_HOME");
        return;
    };
    // FunctionN lives in kotlin-stdlib — needed on the runtime classpath.
    let Some(stdlib) = common::stdlib_jar() else {
        eprintln!("skipping lambda_e2e: no kotlin-stdlib jar found");
        return;
    };
    let src = "fun call1(f: (Int) -> Int, x: Int): Int = f(x)\n\
fun call0(f: () -> Int): Int = f()\n\
fun call2(f: (Int, Int) -> Int): Int = f(20, 22)\n\
fun box(): String {\n\
if (call1({ n -> n + 1 }, 41) != 42) return \"f1\"\n\
if (call0({ 7 }) != 7) return \"f2\"\n\
if (call1({ it * 2 }, 41) != 82) return \"f3\"\n\
if (call2({ a, b -> a + b }) != 42) return \"f4\"\n\
return \"OK\"\n\
}\n";
    let jdk = std::path::PathBuf::from(format!("{java_home}/lib/modules"));
    let Some(out) = common::compile_and_run_box(src, "L", &[stdlib], Some(&jdk)) else {
        return;
    };
    assert_eq!(out, "OK");
}
