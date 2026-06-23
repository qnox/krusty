//! The not-null assertion `x!!` — `kotlin/jvm/internal/Intrinsics.checkNotNull` on a duplicate of the
//! value (yields the value, throwing on null). Round-tripped against the JVM under `-Xverify:all`.

use std::path::PathBuf;

mod common;

#[test]
fn not_null_assert_runs() {
    let Some(java_home) = common::java_home() else {
        eprintln!("skipping not_null_assert_e2e: set JAVA_HOME");
        return;
    };
    let Some(stdlib) = common::stdlib_jar() else {
        eprintln!("skipping not_null_assert_e2e: no kotlin-stdlib jar found");
        return;
    };
    let src = "fun pick(b: Boolean): String? = if (b) \"hi\" else null\n\
fun len(s: String): Int = s.length\n\
fun box(): String {\n\
val x: String? = pick(true)\n\
if (x!! != \"hi\") return \"f1\"\n\
if (len(pick(true)!!) != 2) return \"f2\"\n\
return \"OK\"\n\
}\n";
    let jdk = PathBuf::from(format!("{java_home}/lib/modules"));
    let Some(out) = common::compile_and_run_box(src, "N", &[stdlib], Some(&jdk)) else {
        return;
    };
    assert_eq!(out, "OK");
}
