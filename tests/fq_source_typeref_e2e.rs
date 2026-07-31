//! A fully-qualified SOURCE class name (`pkg1.Cls`, declared in a sibling file of the same module)
//! must resolve in every type position — not just behind an explicit `import`. The signature pass
//! only consulted the import/classpath maps, so a cross-package source FQN like `pkg1.Cls` fell
//! through and the compiler reported `unresolved reference 'pkg1.Cls.'` (first seen on an extension
//! receiver: `fun pkg1.Cls.fn()`). Each test below exercises one type position end-to-end through
//! the front end and asserts it is accepted.

use super::common;

/// Declarations referenced by the "using" snippets below.
const DECLS: &str = "package pkg1\n\
    open class Cls(val n: Int)\n\
    class Outer { class Inner }\n\
    class Box<T>(val v: T)\n";

fn assert_accepted(name: &str, using: &str) {
    let diagnostics = common::front_end_diagnostics_files(&[DECLS, using], &[], None);
    assert!(
        diagnostics.is_empty(),
        "{name}: unexpected diagnostics: {diagnostics:?}"
    );
}

#[test]
fn fq_source_extension_function_receiver() {
    assert_accepted(
        "extension function receiver",
        "package pkg2\nfun pkg1.Cls.describe(): String = \"cls\"\n",
    );
}

#[test]
fn fq_source_extension_property_receiver() {
    assert_accepted(
        "extension property receiver",
        "package pkg2\nval pkg1.Cls.doubled: Int get() = n * 2\n",
    );
}

#[test]
fn fq_source_parameter_type() {
    assert_accepted(
        "parameter type",
        "package pkg2\nfun take(c: pkg1.Cls): Int = c.n\n",
    );
}

#[test]
fn fq_source_return_type() {
    assert_accepted(
        "return type",
        "package pkg2\nfun id(c: pkg1.Cls): pkg1.Cls = c\n",
    );
}

#[test]
fn fq_source_property_type() {
    assert_accepted(
        "property type",
        "package pkg2\nval global: pkg1.Cls? = null\n",
    );
}

#[test]
fn fq_source_type_argument() {
    assert_accepted(
        "type argument",
        "package pkg2\nfun boxed(b: pkg1.Box<pkg1.Cls>): Int = b.v.n\n",
    );
}

#[test]
fn fq_source_generic_bound() {
    assert_accepted(
        "generic bound",
        "package pkg2\nfun <T : pkg1.Cls> pick(t: T): Int = t.n\n",
    );
}

#[test]
fn fq_source_supertype() {
    assert_accepted("supertype", "package pkg2\nclass Sub : pkg1.Cls(1)\n");
}

#[test]
fn fq_source_typealias_target() {
    assert_accepted(
        "typealias target",
        "package pkg2\ntypealias Alias = pkg1.Cls\nfun viaAlias(a: Alias): Int = a.n\n",
    );
}

#[test]
fn fq_source_nested_class_receiver() {
    assert_accepted(
        "nested class receiver",
        "package pkg2\nfun pkg1.Outer.Inner.tag(): String = \"inner\"\n",
    );
}

#[test]
fn fq_source_is_check() {
    assert_accepted(
        "is check",
        "package pkg2\nfun check(x: Any): Boolean = x is pkg1.Cls\n",
    );
}
