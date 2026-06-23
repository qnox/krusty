//! Data-class `copy` with named / omitted arguments, realized via the `$default` mechanism: the JVM
//! backend emits a `copy$default(self, fields…, mask, marker)` stub (byte-identical to kotlinc), and a
//! call with omitted args passes a mask. Round-tripped under `-Xverify:all`.

use std::path::PathBuf;

mod common;

#[test]
fn data_class_copy_runs() {
    let Some(java_home) = common::java_home() else {
        eprintln!("skipping data_copy_e2e: set JAVA_HOME");
        return;
    };
    let Some(stdlib) = common::stdlib_jar() else {
        eprintln!("skipping data_copy_e2e: no kotlin-stdlib jar found");
        return;
    };
    let src = "data class P(val x: Int, val y: String)\n\
fun box(): String {\n\
val p = P(1, \"a\")\n\
val q = p.copy(y = \"b\")\n\
val r = p.copy(x = 9)\n\
val s = p.copy(2, \"c\")\n\
val t = p.copy()\n\
if (q.x != 1 || q.y != \"b\") return \"f1\"\n\
if (r.x != 9 || r.y != \"a\") return \"f2\"\n\
if (s.x != 2 || s.y != \"c\") return \"f3\"\n\
if (t.x != 1 || t.y != \"a\") return \"f4\"\n\
return \"OK\"\n\
}\n";
    let jdk = PathBuf::from(format!("{java_home}/lib/modules"));
    let Some(out) = common::compile_and_run_box(src, "D", &[stdlib], Some(&jdk)) else {
        return;
    };
    assert_eq!(out, "OK");
}
