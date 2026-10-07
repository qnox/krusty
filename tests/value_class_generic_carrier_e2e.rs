//! A value class stored in a generic value class's `T` carrier (`R<R<Int>>`).
//!
//! `value class R<T>(val a: T)` keeps its carrier in an erased `T` slot, so an `R<Int>` stored in
//! it is the BOX, as it is in any generic slot. Constructing `R<R<Int>>(a)` boxes `a` for
//! `constructor-impl(Object)`, and reading `c.a` into a reference consumer (`Any?`, or another
//! generic parameter) hands that box on without unboxing and boxing it again. Both shapes match
//! kotlinc's code.

use super::common;

const SRC: &str = "@JvmInline value class R<T>(val a: T)\n\
    @JvmInline value class S(val text: String)\n\
    fun make(a: R<Int>): R<R<Int>> = R<R<Int>>(a)\n\
    fun makeNested(): R<R<Int>> = R<R<Int>>(R<Int>(1))\n\
    fun read(c: R<R<Int>>): Any? = c.a\n\
    fun readTyped(c: R<R<Int>>): R<Int> = c.a\n\
    fun <T> same(a: T, b: T): Boolean = a == b\n\
    fun compare(c: R<S>): Boolean = same(S(\"k\"), c.a)\n\
    fun box(): String {\n\
    \x20   val c = make(R<Int>(7))\n\
    \x20   if (read(c) !is R<*>) return \"fail: stored carrier\"\n\
    \x20   if (readTyped(c).a != 7) return \"fail: typed read\"\n\
    \x20   if (makeNested().a.a != 1) return \"fail: nested construction\"\n\
    \x20   if (!compare(R<S>(S(\"k\")))) return \"fail: generic consumer\"\n\
    \x20   return \"OK\"\n\
    }\n";

/// The source file is `Main.kt`, so the functions live in the facade `MainKt`.
fn assert_same_method(method: &str) {
    match common::class_bytes_diff_against_kotlinc("Main", &[], SRC, "MainKt", method) {
        Some(Ok(())) => {}
        Some(Err(diff)) => panic!("{diff}"),
        None => panic!("{method}: reference toolchain unavailable"),
    }
}

#[test]
fn constructing_a_generic_carrier_boxes_the_value_class_argument() {
    assert_same_method("public static final java.lang.Object make-");
}

#[test]
fn a_nested_construction_boxes_the_inner_value() {
    assert_same_method("public static final java.lang.Object makeNested(");
}

#[test]
fn a_generic_carrier_read_into_a_reference_keeps_the_box() {
    assert_same_method("public static final java.lang.Object read-");
}

#[test]
fn a_generic_carrier_read_as_its_value_class_unboxes_once() {
    assert_same_method("public static final java.lang.Object readTyped-");
}

#[test]
fn a_generic_carrier_read_into_a_generic_parameter_keeps_the_box() {
    assert_same_method("public static final boolean compare-");
}

#[test]
fn values_stored_in_a_generic_carrier_keep_their_boxes() {
    let output = common::compile_and_run_box(SRC, "Main", &[common::stdlib_jar()], None)
        .expect("krusty compiles and the JVM runs the box function");
    assert_eq!(output, "OK");
}
