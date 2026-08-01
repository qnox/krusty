//! Omitted default arguments on MEMBER EXTENSION functions (`class A { fun Int.foo(a: Int = 1) }`,
//! called `1.foo()`).
//!
//! The checker only recorded `resolved_call_arg_slots` for member-extension calls with NAMED
//! arguments — a positional omitted-default call recorded nothing, so lowering had no slot map and
//! bailed the file. The `$default` stub and its emit path already existed (they power the named
//! form); the checker now mirrors the plain member path's condition (named OR trailing-lambda OR
//! omitted-with-defaults).

use super::common;

fn run_box(src: &str, stem: &str) {
    let Some(out) = common::compile_and_run_with_stdlib(src, stem) else {
        panic!("{stem}: expected the box to compile and run");
    };
    assert_eq!(out, "OK", "{stem}");
}

/// Inner-class member extension, single default, omitted and provided.
#[test]
fn inner_member_ext_default_omitted() {
    run_box(
        r#"
class A {
    fun Int.foo(a: Int = 1): Int = a

    fun test(): String {
        if (1.foo() != 1) return "f1"
        if (1.foo(2) != 2) return "f2"
        return "OK"
    }
}

fun box(): String = A().test()
"#,
        "InnerExtDefault",
    );
}

/// Two defaults, all omission combinations incl. named.
#[test]
fn inner_member_ext_two_defaults() {
    run_box(
        r#"
class A {
    fun Double.foo(a: Double = 1.0, b: Double = 1.0): Double = a + b

    fun test(): String {
        if (1.0.foo() != 2.0) return "f1"
        if (1.0.foo(2.0, 2.0) != 4.0) return "f2"
        if (1.0.foo(a = 2.0) != 3.0) return "f3"
        if (1.0.foo(b = 2.0) != 3.0) return "f4"
        return "OK"
    }
}

fun box(): String = A().test()
"#,
        "InnerExtTwoDefaults",
    );
}

/// The same on an `object` and a `private` member extension.
#[test]
fn object_and_private_member_ext_defaults() {
    run_box(
        r#"
object A {
    fun Int.foo(a: Int = 1): Int = a

    fun test(): String {
        if (1.foo() != 1) return "f1"
        if (1.foo(2) != 2) return "f2"
        return "OK"
    }
}

fun box(): String = A.test()
"#,
        "ObjectExtDefault",
    );
    run_box(
        r#"
class A {
    private fun Int.foo(a: Int = 1): Int = a

    fun test(): String {
        if (1.foo() != 1) return "f1"
        if (1.foo(2) != 2) return "f2"
        return "OK"
    }
}

fun box(): String = A().test()
"#,
        "PrivateExtDefault",
    );
}

/// A default that reads the extension receiver (`= this`-dependent shapes stay constant here).
#[test]
fn inner_member_ext_default_reordering() {
    run_box(
        r#"
class A {
    fun Int.sum(a: Int, b: Int = a, c: Int = b): Int = a + b + c

    fun test(): String {
        if (1.sum(3) != 9) return "f1:${1.sum(3)}"
        if (1.sum(3, 5) != 13) return "f2"
        if (1.sum(3, c = 10) != 16) return "f3"
        return "OK"
    }
}

fun box(): String = A().test()
"#,
        "InnerExtDefaultChain",
    );
}

/// The exact corpus cases.
#[test]
fn corpus_member_ext_defaults_box_ok() {
    if !common::corpus_ready() {
        return;
    }
    for case in [
        "defaultArguments/function/innerExtentionFunction.kt",
        "defaultArguments/function/innerExtentionFunctionDouble.kt",
        "defaultArguments/function/innerExtentionFunctionDoubleTwoArgs.kt",
        "defaultArguments/function/innerExtentionFunctionManyArgs.kt",
        "defaultArguments/function/extentionFunctionInObject.kt",
        "defaultArguments/private/memberExtensionFunction.kt",
    ] {
        assert_eq!(
            common::run_box_corpus_case(case).as_deref(),
            Some("OK"),
            "{case} must execute successfully, not silently skip"
        );
    }
}

/// REJECTION GUARD: a trailing-lambda call that OMITS a vararg (`1.foo {}` on
/// `fun Int.foo(vararg xs: Int, block: () -> Unit)`) must NOT route to a `$default` stub — the
/// empty-array vararg default is only synthesized when some param carries an explicit default, so
/// the stub may not exist (a dangling `foo$default` call → NoSuchMethodError). Covers both the
/// member-extension path this file's feature enabled and the pre-existing plain-member path.
#[test]
fn trailing_lambda_vararg_omission_no_dangling_default() {
    let Some(jdk) = common::jdk_modules() else {
        return;
    };
    let cases: &[(&str, &str)] = &[
        (
            "MemberExtVarargTrailing",
            r#"
class A {
    fun Int.foo(vararg xs: Int, block: () -> Unit): String {
        block()
        return "OK"
    }
    fun test(): String = 1.foo {}
}
fun box(): String = A().test()
"#,
        ),
        (
            "MemberVarargTrailing",
            r#"
class A {
    fun foo(vararg xs: Int, block: () -> Unit): String {
        block()
        return "OK"
    }
    fun test(): String = foo {}
}
fun box(): String = A().test()
"#,
        ),
    ];
    for (stem, src) in cases {
        let cp = krusty::toolchain::classpath_jars_for(src);
        let outcome = common::backend_outcome_in_process(src, stem, &cp, Some(&jdk));
        assert_ne!(
            outcome,
            Some(common::BackendOutcome::Emitted),
            "{stem}: omitted-vararg trailing-lambda call must not emit a dangling $default call"
        );
    }
}
