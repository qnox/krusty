//! A reference to a CLASSPATH top-level function — `::greet` where `greet` lives in a jar / dependency
//! module — was rejected ("callable references are not supported"). Now it resolves to a
//! `FunctionReferenceImpl` whose `invoke` calls the real `invokestatic <facade>.greet(args)`. Verified
//! end-to-end on a real JVM (the function is in a separately kotlinc-compiled library).
use super::common;
fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.is_empty())
}
#[test]
fn classpath_function_reference_compiles_and_runs() {
    let Some(jh) = env("KRUSTY_REF_JAVA_HOME").or_else(|| env("JAVA_HOME")) else {
        eprintln!("skipping: set JAVA_HOME");
        return;
    };
    let Some(sl) = common::stdlib_jar() else {
        eprintln!("skipping: no kotlin-stdlib jar");
        return;
    };
    let jdk = std::path::PathBuf::from(format!("{jh}/lib/modules"));
    let Some(libout) =
        common::compile_lib("cfr", "package lib\nfun add(a: Int, b: Int): Int = a + b\n")
    else {
        return;
    };
    let cp = vec![libout.clone(), sl.clone()];
    // `::add` bound to a val and invoked, and passed to a higher-order function.
    let main = "import lib.add\n\
        fun apply2(f: (Int, Int) -> Int): Int = f(10, 20)\n\
        fun box(): String {\n\
        \x20 val g = ::add\n\
        \x20 return if (g(3, 4) == 7 && apply2(::add) == 30) \"OK\" else \"fail\"\n\
        }\n";
    let classes = common::compile_in_process(main, "Main", &cp, Some(&jdk))
        .expect("krusty failed to compile a classpath function reference");
    match common::run_box(&classes, "MainKt", &[libout, sl]) {
        Some(o) => assert_eq!(o.trim(), "OK", "box() = {o:?}"),
        None => eprintln!("skipping: box runner unavailable"),
    }
}
