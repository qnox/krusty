//! A method that declares a type parameter SHADOWING its class's (`class Box<T> { fun <T> m(x: T): T }`)
//! must resolve the method's return against the METHOD's `T`, not the class's. Calling `b.m("hi")` on a
//! `Box<Int>` yields `String` (the argument), not `Int` (the class arg). Before the shadow guard the
//! member-return substitution wrongly bound the class type argument, so `val r: String = b.m("hi")` was
//! rejected as "inferred Int but String expected". Round-tripped under `-Xverify:all`.

mod common;

#[test]
fn method_type_param_shadows_class_one() {
    let Some(java_home) = common::java_home() else {
        eprintln!("skipping method_shadows_class_type_param_e2e: set JAVA_HOME");
        return;
    };
    let Some(stdlib) = common::stdlib_jar() else {
        eprintln!("skipping method_shadows_class_type_param_e2e: no kotlin-stdlib jar found");
        return;
    };
    let src = "class Box<T>(val v: T) { fun <T> m(x: T): T = x }\n\
fun box(): String {\n\
val b: Box<Int> = Box(1)\n\
val r: String = b.m(\"hi\")\n\
if (r != \"hi\") return \"f1\"\n\
if (b.v + 1 != 2) return \"f2\"\n\
return \"OK\"\n\
}\n";
    let jdk = std::path::PathBuf::from(format!("{java_home}/lib/modules"));
    let out = common::compile_and_run_box(src, "B", &[stdlib], Some(&jdk)).expect(
        "krusty must resolve a method type param shadowing the class's (Box<Int>.m(\"hi\"))",
    );
    assert_eq!(out, "OK");
}
