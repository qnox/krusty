//! A value-class default argument of a regular class's primary constructor.
//!
//! The synthetic `<init>(…, int, DefaultConstructorMarker)` fills an omitted `S` parameter with
//! its default expression, which runs over the primary constructor's parameters like the class's
//! other constructor-owned code. `S("K")` there is a value construction, so it becomes
//! `S.constructor-impl` and stores the carrier, not a `new S` box, as kotlinc does.

use super::common;

const SRC: &str = "@JvmInline value class S(val text: String)\n\
    class Plain(val x: S, val y: S = S(\"K\"))\n\
    class Derived(val x: S, val y: S = S(x.text + \"K\"))\n\
    class Generic<T>(val t: T, val y: S = S(\"G\"))\n\
    fun box(): String {\n\
    \x20   if (Plain(S(\"O\")).y.text != \"K\") return \"fail: plain\"\n\
    \x20   if (Derived(S(\"O\")).y.text != \"OK\") return \"fail: derived\"\n\
    \x20   if (Generic(1).y.text != \"G\") return \"fail: generic\"\n\
    \x20   return \"OK\"\n\
    }\n";

fn assert_same_defaults_constructor(class: &str, method: &str) {
    match common::class_bytes_diff_against_kotlinc("Main", &[], SRC, class, method) {
        Some(Ok(())) => {}
        Some(Err(diff)) => panic!("{diff}"),
        None => panic!("{class}.{method}: reference toolchain unavailable"),
    }
}

#[test]
fn a_constant_value_class_default_constructs_the_carrier() {
    assert_same_defaults_constructor(
        "Plain",
        "public Plain(java.lang.String, java.lang.String, int, kotlin.jvm.internal.DefaultConstructorMarker)",
    );
}

#[test]
fn a_value_class_default_reads_an_earlier_value_class_parameter() {
    assert_same_defaults_constructor(
        "Derived",
        "public Derived(java.lang.String, java.lang.String, int, kotlin.jvm.internal.DefaultConstructorMarker)",
    );
}

#[test]
fn a_generic_class_value_class_default_constructs_the_carrier() {
    assert_same_defaults_constructor(
        "Generic",
        "public Generic(java.lang.Object, java.lang.String, int, kotlin.jvm.internal.DefaultConstructorMarker)",
    );
}

#[test]
fn value_class_constructor_defaults_run() {
    let output = common::compile_and_run_box(
        SRC,
        "Main",
        &[common::stdlib_jar()],
        Some(&common::jdk_modules()),
    )
    .expect("krusty compiles and the JVM runs the box function");
    assert_eq!(output, "OK");
}
