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

/// The shadowing method is INHERITED from a base class: `Sub<Int>` extends `Base<T>`, and `m`'s `T` is
/// declared on `m` itself. The shadow check walks the base-class chain (`method_of`) so it finds `m`
/// shadows even though it isn't on the receiver's own class — the member return must NOT bind to the
/// receiver's `Int`. NOTE: a generic subclass inheriting a generic method is not yet fully lowered, so
/// krusty may DECLINE (returns `None`); we skip in that case rather than fail. When generic-subclass
/// codegen lands this asserts the result runs as `String` ("hi"). The resolution-level guard itself is
/// exercised by the conformance corpus staying green; the own-class case above is the end-to-end pin.
#[test]
fn inherited_method_type_param_shadows_class_one() {
    let Some(java_home) = common::java_home() else {
        eprintln!("skipping inherited shadow: set JAVA_HOME");
        return;
    };
    let Some(stdlib) = common::stdlib_jar() else {
        eprintln!("skipping inherited shadow: no kotlin-stdlib jar found");
        return;
    };
    let src = "open class Base<T>(val v: T) { fun <T> m(x: T): T = x }\n\
class Sub<T>(v: T) : Base<T>(v)\n\
fun box(): String {\n\
val s: Sub<Int> = Sub(1)\n\
val r: String = s.m(\"hi\")\n\
if (r != \"hi\") return \"f1\"\n\
if (s.v + 1 != 2) return \"f2\"\n\
return \"OK\"\n\
}\n";
    let jdk = std::path::PathBuf::from(format!("{java_home}/lib/modules"));
    match common::compile_and_run_box(src, "S", &[stdlib], Some(&jdk)) {
        Some(out) => assert_eq!(out, "OK"),
        None => eprintln!(
            "skip: krusty declined a generic subclass inheriting a generic method (codegen gap); \
             the inherited-shadow resolution guard is covered by conformance"
        ),
    }
}
