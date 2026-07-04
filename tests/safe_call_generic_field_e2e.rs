//! Safe call `?.` reading a GENERIC backing field (`box?.v` where `v: T` on `class Box<T>`). The field
//! erases to `Object` on the JVM, so the read needs a `checkcast`/unbox to the concrete instantiation
//! type — the same coercion the non-safe `.` path applies. Before the `lower_field_read_on` consolidation
//! the `?.` path skipped that coercion, so a chained use (`box?.v.length`, an `Int` field arithmetic)
//! would leave an erased `Object` on the stack. Round-tripped under `-Xverify:all`.

use super::common;

#[test]
fn safe_call_generic_field_coerces() {
    let Some(java_home) = common::java_home() else {
        eprintln!("skipping safe_call_generic_field_e2e: set JAVA_HOME");
        return;
    };
    let Some(stdlib) = common::stdlib_jar() else {
        eprintln!("skipping safe_call_generic_field_e2e: no kotlin-stdlib jar found");
        return;
    };
    // `bs?.v` is a `String?` → `.length` after `?:` ; `bi?.v` is an `Int?` (boxed) → unboxed for `+`.
    let src = "class Box<T>(val v: T)\n\
fun mk(b: Boolean): Box<String>? = if (b) Box(\"hello\") else null\n\
fun mkInt(b: Boolean): Box<Int>? = if (b) Box(40) else null\n\
fun box(): String {\n\
val n = mk(true)?.v?.length ?: -1\n\
if (n != 5) return \"f1\"\n\
if ((mk(false)?.v?.length ?: -1) != -1) return \"f2\"\n\
val k = mkInt(true)?.v ?: 0\n\
if (k + 2 != 42) return \"f3\"\n\
return \"OK\"\n\
}\n";
    let jdk = std::path::PathBuf::from(format!("{java_home}/lib/modules"));
    let out = common::compile_and_run_box(src, "B", &[stdlib], Some(&jdk))
        .expect("krusty must compile a safe-call read of a generic field (box?.v)");
    assert_eq!(out, "OK");
}
