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
    // Regression guard: this position already resolved pre-fix (via alias expansion).
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
    // Regression guard: this position already resolved pre-fix (the checker's own fallback).
    assert_accepted(
        "is check",
        "package pkg2\nfun check(x: Any): Boolean = x is pkg1.Cls\n",
    );
}

#[test]
fn fq_source_unresolved_path_still_errors() {
    let diagnostics = common::front_end_diagnostics_files(
        &[DECLS, "package pkg2\nfun pkg1.Nope.fn() {}\n"],
        &[],
        None,
    );
    assert!(
        diagnostics
            .iter()
            .any(|d| d.contains("unresolved reference 'pkg1.Nope'")),
        "expected an unresolved-reference diagnostic, got: {diagnostics:?}"
    );
}

/// A class named `Cls` exists in BOTH `pkg1` (member `n`) and `pkg3` (member `other`):
/// `pkg1.Cls` must bind the named package's declaration, not just any same-named class.
/// Asserted via TYPE IDENTITY (a mismatched return type errors both ways) rather than member
/// access — member lookup on same-named cross-package classes is a separate pre-existing bug.
const OTHER_CLS: &str = "package pkg3\nclass Cls(val other: Int)\n";

#[test]
fn fq_source_binds_the_named_package() {
    let diagnostics = common::front_end_diagnostics_files(
        &[
            DECLS,
            OTHER_CLS,
            "package pkg2\nfun f(c: pkg1.Cls): pkg1.Cls = c\n",
        ],
        &[],
        None,
    );
    assert!(diagnostics.is_empty(), "identity return: {diagnostics:?}");
    for (from, to) in [("pkg1", "pkg3"), ("pkg3", "pkg1")] {
        let diagnostics = common::front_end_diagnostics_files(
            &[
                DECLS,
                OTHER_CLS,
                &format!("package pkg2\nfun f(c: {from}.Cls): {to}.Cls = c\n"),
            ],
            &[],
            None,
        );
        assert!(
            diagnostics.iter().any(|d| d.contains("type mismatch")),
            "expected a type mismatch returning {from}.Cls as {to}.Cls, got: {diagnostics:?}"
        );
    }
}

#[test]
fn fq_source_nested_classifier_shadows_package_path() {
    // kotlinc resolves a dotted path classifier-first: a root-package class `pkg1` with a nested
    // `Cls` shadows package `pkg1`'s `Cls` for the reference `pkg1.Cls` in a root-package file.
    let pkg = "package pkg1\nclass Cls(val fromPkg: Int)\n";
    let nested = "class pkg1 { class Cls(val fromNested: Int) }\n";
    let diagnostics = common::front_end_diagnostics_files(
        &[pkg, nested, "fun f(c: pkg1.Cls): Int = c.fromPkg\n"],
        &[],
        None,
    );
    assert!(
        diagnostics.iter().any(|d| d.contains("fromPkg")),
        "expected `fromPkg` (package path) to be unresolved — the nested classifier shadows it, \
         got: {diagnostics:?}"
    );
    let diagnostics = common::front_end_diagnostics_files(
        &[pkg, nested, "fun f(c: pkg1.Cls): Int = c.fromNested\n"],
        &[],
        None,
    );
    assert!(
        diagnostics.is_empty(),
        "nested classifier member: {diagnostics:?}"
    );
}

#[test]
fn fq_source_typerefs_compile_and_run() {
    // FQ param + return types across packages, exercised end-to-end on the JVM. (A cross-package
    // extension DECLARATION would be the ideal vehicle, but the IR backend rejects those today —
    // a pre-existing limitation, unrelated to FQ name resolution.)
    common::expect_box_ok_files_with_stdlib(
        &[
            (
                "pkg1/Cls",
                "package pkg1\nclass Cls(val n: Int)\nfun Cls.doubled(): Int = n * 2\n",
            ),
            (
                "pkg2/Use",
                "package pkg2\nimport pkg1.Cls\nimport pkg1.doubled\n\
                 fun shrink(c: pkg1.Cls): pkg1.Cls = Cls(c.n / 2)\n\
                 fun box(): String = if (shrink(Cls(42)).doubled() == 42) \"OK\" else \"fail\"\n",
            ),
        ],
        "fq_typerefs",
    );
}
