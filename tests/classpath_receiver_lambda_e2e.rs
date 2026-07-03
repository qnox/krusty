//! A lambda passed to a CLASSPATH (separately-compiled) function's RECEIVER function-type parameter
//! (`build(b: Box.() -> Unit)` in a dependency jar) binds its implicit `this` to the receiver, so a bare
//! member call inside resolves against it. krusty decodes the `@ExtensionFunctionType` annotation + the
//! receiver type argument from the callee's `@Metadata` (emitted by real kotlinc). Round-tripped on a JVM.
mod common;
fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.is_empty())
}
#[test]
fn classpath_receiver_lambda_compiles_and_runs() {
    let Some(jh) = env("KRUSTY_REF_JAVA_HOME").or_else(|| env("JAVA_HOME")) else {
        eprintln!("skipping: set JAVA_HOME");
        return;
    };
    let Some(sl) = common::stdlib_jar() else {
        eprintln!("skipping: no kotlin-stdlib jar");
        return;
    };
    let jdk = std::path::PathBuf::from(format!("{jh}/lib/modules"));
    let Some(libout) = common::compile_lib(
        "crl",
        "package lib\n\
         class Box { var v: Int = 0; fun set(x: Int) { v = x } }\n\
         fun build(b: Box.() -> Unit): Box { val box = Box(); box.b(); return box }\n",
    ) else {
        return;
    };
    let cp = vec![libout.clone(), sl.clone()];
    // `build { set(42) }` — `set` is a member of the lambda's implicit `this: Box`, from the classpath.
    let main = "import lib.build\n\
        fun box(): String {\n\
        \x20 val r = build { set(42) }\n\
        \x20 return if (r.v == 42) \"OK\" else \"FAIL ${r.v}\"\n\
        }\n";
    let classes = common::compile_in_process(main, "Main", &cp, Some(&jdk))
        .expect("krusty failed to compile a classpath receiver-lambda");
    match common::run_box(&classes, "MainKt", &[libout, sl]) {
        Some(o) => assert_eq!(o.trim(), "OK", "box() = {o:?}"),
        None => eprintln!("skipping: box runner unavailable"),
    }
}
