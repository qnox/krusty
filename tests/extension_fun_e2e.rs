//! Top-level extension functions `fun Recv.name(…)` — compiled as static methods whose first
//! parameter is the receiver (Kotlin's strategy). Same-named extensions on different receivers don't
//! collide (dispatched by receiver). A user `operator fun` extension overrides the builtin operator.
//! Round-tripped under `-Xverify:all`.

use std::path::PathBuf;

mod common;

#[test]
fn extension_functions_run() {
    let Some(java_home) = common::java_home() else {
        eprintln!("skipping extension_fun_e2e: set JAVA_HOME");
        return;
    };
    let Some(stdlib) = common::stdlib_jar() else {
        eprintln!("skipping extension_fun_e2e: no kotlin-stdlib jar found");
        return;
    };
    let src = "fun Int.dbl(): Int = this * 2\n\
fun String.dbl(): String = this + this\n\
fun Int.plusX(x: Int): Int = this + x\n\
fun box(): String {\n\
if (3.dbl() != 6) return \"f1\"\n\
if (\"a\".dbl() != \"aa\") return \"f2\"\n\
if (3.plusX(4) != 7) return \"f3\"\n\
return \"OK\"\n\
}\n";
    let jdk = PathBuf::from(format!("{java_home}/lib/modules"));
    let Some(out) = common::compile_and_run_box(src, "D", &[stdlib], Some(&jdk)) else {
        return;
    };
    assert_eq!(out, "OK");
}
