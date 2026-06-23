//! Default property accessors: a class's backing fields are private, with synthesized `getX()`/`setX()`
//! accessors; access from outside the declaring class goes through them (`c.x`/`c.x = v`), while inside
//! the class the field is used directly. Round-tripped under `-Xverify:all`.

use std::path::PathBuf;

mod common;

#[test]
fn property_accessors_run() {
    let Some(java_home) = common::java_home() else {
        eprintln!("skipping property_accessor_e2e: set JAVA_HOME");
        return;
    };
    let Some(stdlib) = common::stdlib_jar() else {
        eprintln!("skipping property_accessor_e2e: no kotlin-stdlib jar found");
        return;
    };
    let src = "class Box(val x: Int, var y: String) {\n\
    fun internal(): Int = x\n\
}\n\
fun box(): String {\n\
val b = Box(10, \"a\")\n\
if (b.x != 10) return \"f1\"\n\
if (b.internal() != 10) return \"f2\"\n\
b.y = \"z\"\n\
if (b.y != \"z\") return \"f3\"\n\
return \"OK\"\n\
}\n";
    let jdk = PathBuf::from(format!("{java_home}/lib/modules"));
    let Some(out) = common::compile_and_run_box(src, "B", &[stdlib], Some(&jdk)) else {
        return;
    };
    assert_eq!(out, "OK");
}
