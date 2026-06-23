//! `vararg` parameters — the call site packs trailing arguments into a fresh array (`newarray` +
//! element stores) passed as the array parameter — plus `for (x in arr)` array iteration to consume
//! it. Round-tripped against the JVM under `-Xverify:all`.

mod common;

#[test]
fn vararg_and_array_iteration_run() {
    let Some(java_home) = common::java_home() else {
        eprintln!("skipping vararg_e2e: set JAVA_HOME");
        return;
    };
    let Some(stdlib) = common::stdlib_jar() else {
        eprintln!("skipping vararg_e2e: no kotlin-stdlib jar found");
        return;
    };
    let src = "fun sum(vararg xs: Int): Int { var s = 0; for (x in xs) s += x; return s }\n\
fun concat(vararg ss: String): String { var r = \"\"; for (s in ss) r = r + s; return r }\n\
fun box(): String {\n\
if (sum(1, 2, 3, 4) != 10) return \"f1\"\n\
if (sum() != 0) return \"f2\"\n\
if (concat(\"a\", \"b\", \"c\") != \"abc\") return \"f3\"\n\
return \"OK\"\n\
}\n";
    let jdk = std::path::PathBuf::from(format!("{java_home}/lib/modules"));
    let Some(out) = common::compile_and_run_box(src, "V", &[stdlib], Some(&jdk)) else {
        return;
    };
    assert_eq!(out, "OK");
}
