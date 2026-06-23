//! `throw` of a classpath exception constructed via `IrExpr::NewExternal` (`new` + `<init>` resolved
//! from the classpath), plus `athrow`. Round-tripped against the JVM under `-Xverify:all`.

mod common;

#[test]
fn throw_runs() {
    let Some(java_home) = common::java_home() else {
        eprintln!("skipping throw_e2e: set JAVA_HOME");
        return;
    };
    let Some(stdlib) = common::stdlib_jar() else {
        eprintln!("skipping throw_e2e: no kotlin-stdlib jar found");
        return;
    };
    let src = "fun check(b: Boolean): Int { if (b) throw RuntimeException(\"bad\"); return 7 }\n\
fun box(): String {\n\
if (check(false) != 7) return \"f1\"\n\
val e = IllegalStateException(\"unused\")\n\
return \"OK\"\n\
}\n";
    let jdk = std::path::PathBuf::from(format!("{java_home}/lib/modules"));
    let Some(out) = common::compile_and_run_box(src, "T", &[stdlib], Some(&jdk)) else {
        return;
    };
    assert_eq!(out, "OK");
}
