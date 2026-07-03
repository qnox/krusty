//! A call to a CLASSPATH (separately-compiled) function that OMITS a defaulted argument — `host("x")`
//! where `host(a: String, b: Int = 7)` lives in a dependency module — was rejected ("unresolved
//! function"), because krusty didn't recover the per-parameter `DECLARES_DEFAULT_VALUE` flag from the
//! callee's `@Metadata`. Now the omitted trailing default resolves and lowers to the `host$default`
//! synthetic. Verified end-to-end on a real JVM (the function is in a separately kotlinc-compiled lib).
mod common;
fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.is_empty())
}
#[test]
fn classpath_default_arg_omission_compiles_and_runs() {
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
        "cda",
        "package lib\nfun host(a: String, b: Int = 7): String = a + b\n",
    ) else {
        return;
    };
    let cp = vec![libout.clone(), sl.clone()];
    // `host("x")` omits the defaulted `b` (= 7) → "x7"; `host("y", 3)` supplies it → "y3".
    let main = "import lib.host\n\
        fun box(): String {\n\
        \x20 if (host(\"x\") != \"x7\") return \"fail omit: ${host(\"x\")}\"\n\
        \x20 if (host(\"y\", 3) != \"y3\") return \"fail supply: ${host(\"y\", 3)}\"\n\
        \x20 return \"OK\"\n\
        }\n";
    let classes = common::compile_in_process(main, "Main", &cp, Some(&jdk))
        .expect("krusty failed to compile a classpath default-argument omission");
    match common::run_box(&classes, "MainKt", &[libout, sl]) {
        Some(o) => assert_eq!(o.trim(), "OK", "box() = {o:?}"),
        None => eprintln!("skipping: box runner unavailable"),
    }
}
