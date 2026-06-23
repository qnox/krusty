//! Computed properties (custom getter, no backing field): top-level `val x get() = …` → static
//! `getX()`; class `val y get() = …` → instance `getX()` (`obj.y`/unqualified `y`). Round-tripped
//! under `-Xverify:all`.

mod common;

#[test]
fn computed_properties_run() {
    let Some(java_home) = common::java_home() else {
        eprintln!("skipping computed_prop_e2e: set JAVA_HOME");
        return;
    };
    let Some(stdlib) = common::stdlib_jar() else {
        eprintln!("skipping computed_prop_e2e: no kotlin-stdlib jar found");
        return;
    };
    const SRC: &str = "val top: Int get() = 42\n\
class C(val a: Int, val b: Int) {\n\
    val sum: Int get() = a + b\n\
    val label: String get() = \"v\" + sum\n\
    fun viaThis(): Int = sum\n\
}\n\
fun box(): String {\n\
if (top != 42) return \"f1\"\n\
val c = C(2, 3)\n\
if (c.sum != 5) return \"f2\"\n\
if (c.viaThis() != 5) return \"f3\"\n\
if (c.label != \"v5\") return \"f4\"\n\
return \"OK\"\n\
}\n";
    let jdk = std::path::PathBuf::from(format!("{java_home}/lib/modules"));
    let Some(out) = common::compile_and_run_box(SRC, "P", &[stdlib], Some(&jdk)) else {
        return;
    };
    assert_eq!(out, "OK");
}
