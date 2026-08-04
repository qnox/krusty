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

/// Trailing-lambda calls route a non-last vararg through one semantic packing path: omission materializes
/// the declared empty array, while multiple positional elements are collected into that same physical
/// array. Both member extensions and plain members exercise omission and multiple elements. Inspecting
/// primitive `IntArray` contents prevents an erased `Object[]` placeholder or a first-element-only slot
/// map from hiding in the result.
#[test]
fn trailing_lambda_vararg_omission_no_dangling_default() {
    let cases: &[(&str, &str)] = &[
        (
            "MemberExtVarargTrailing",
            r#"
class A {
    fun Int.foo(vararg xs: Int, block: () -> Unit): String {
        block()
        if (xs.isEmpty()) return "empty"
        if (xs.size == 2 && xs[0] == 2 && xs[1] == 3) return "packed"
        return "wrong"
    }
    fun test(): String {
        if (1.foo {} != "empty") return "empty"
        if (1.foo(2, 3) {} != "packed") return "packed"
        return "OK"
    }
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
        if (xs.isEmpty()) return "empty"
        if (xs.size == 2 && xs[0] == 2 && xs[1] == 3) return "packed"
        return "wrong"
    }
    fun test(): String {
        if (foo {} != "empty") return "empty"
        if (this.foo(2, 3) {} != "packed") return "packed"
        return "OK"
    }
}
fun box(): String = A().test()
"#,
        ),
    ];
    for (stem, src) in cases {
        let cp = krusty::toolchain::classpath_jars_for(src);
        let jdk = common::jdk_modules();
        assert_eq!(
            common::backend_outcome_in_process(src, stem, &cp, Some(jdk.as_path())),
            Some(common::BackendOutcome::Emitted),
            "{stem}: valid omitted vararg must reach backend emission"
        );
        run_box(src, stem);
    }
}

/// When another parameter really is defaulted, the omitted vararg still supplies an empty array but
/// only the explicit default receives a mask bit in the `$default` call.
#[test]
fn trailing_lambda_omits_vararg_and_explicit_default() {
    run_box(
        r#"
class A {
    fun Int.foo(prefix: String = "O", vararg xs: Int, block: () -> String): String {
        if (xs.isNotEmpty()) return "elements"
        return prefix + block()
    }
    fun test(): String = 1.foo { "K" }
}
fun box(): String = A().test()
"#,
        "MemberExtVarargAndDefault",
    );
}
