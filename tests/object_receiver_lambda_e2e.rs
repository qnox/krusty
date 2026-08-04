//! Lambda arguments to members called on an *object* (or companion object) receiver must receive
//! the selected candidate's parameter types, exactly as an instance receiver does — so implicit
//! `it` binds to the declared function-type parameter instead of `Any`.
//!
//! Also covers `Type { … }` where `Type` is a **source** class whose companion declares
//! `operator fun invoke`: when the lambda is not applicable to any constructor, the companion
//! `invoke` is selected (the classpath equivalent already worked via `classpath_companion_ty`).

use super::common;

fn expect_ok(src: &str, stem: &str) {
    common::expect_box_ok_with_stdlib(src, stem);
}

/// The checker's diagnostics for `src`, or `None` when the toolchain is unprovisioned.
fn diags(src: &str) -> Option<Vec<String>> {
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    Some(common::front_end_diagnostics(src, &[stdlib], Some(&jdk)))
}

#[test]
fn object_receiver_lambda_param_types() {
    const SRC: &str = "object Wrap {\n\
    fun apply2(f: (Int) -> Int): Int = f(5)\n\
}\n\
fun box(): String {\n\
    val r = Wrap.apply2 { it * 2 }\n\
    return if (r == 10) \"OK\" else \"FAIL: $r\"\n\
}\n";
    expect_ok(SRC, "ObjectReceiverLambda");
}

#[test]
fn companion_receiver_lambda_param_types() {
    const SRC: &str = "class Holder {\n\
    companion object {\n\
        fun apply2(f: (Int) -> Int): Int = f(5)\n\
    }\n\
}\n\
fun box(): String {\n\
    val r = Holder.apply2 { it * 2 }\n\
    return if (r == 10) \"OK\" else \"FAIL: $r\"\n\
}\n";
    expect_ok(SRC, "CompanionReceiverLambda");
}

/// An unqualified call to a sibling companion function from INSIDE the companion: the receiver is
/// the companion object, so the lambda's parameter types come from that member's signature.
#[test]
fn companion_internal_receiver_lambda_param_types() {
    const SRC: &str = "class Holder {\n\
    companion object {\n\
        fun apply2(f: (Int) -> Int): Int = f(5)\n\
        fun inside(): Int = apply2 { it * 2 }\n\
    }\n\
}\n\
fun box(): String {\n\
    val r = Holder.inside()\n\
    return if (r == 10) \"OK\" else \"FAIL: $r\"\n\
}\n";
    expect_ok(SRC, "CompanionInternalReceiverLambda");
}

/// The same shape on an `object`: the unqualified call's implicit receiver is the singleton.
#[test]
fn object_internal_receiver_lambda_param_types() {
    const SRC: &str = "object Wrap {\n\
    fun apply2(f: (Int) -> Int): Int = f(5)\n\
    fun inside(): Int = apply2 { it * 2 }\n\
}\n\
fun box(): String {\n\
    val r = Wrap.inside()\n\
    return if (r == 10) \"OK\" else \"FAIL: $r\"\n\
}\n";
    expect_ok(SRC, "ObjectInternalReceiverLambda");
}

#[test]
fn object_receiver_lambda_two_params() {
    const SRC: &str = "object Wrap {\n\
    fun combine(f: (Int, String) -> String): String = f(2, \"x\")\n\
}\n\
fun box(): String {\n\
    val r = Wrap.combine { n, s -> s.repeat(n) }\n\
    return if (r == \"xx\") \"OK\" else \"FAIL: $r\"\n\
}\n";
    expect_ok(SRC, "ObjectReceiverLambdaTwoParams");
}

/// A RECEIVER function type (`Int.() -> Int`) binds the lambda's `this`, not `it`.
#[test]
fn object_receiver_lambda_with_receiver_param() {
    const SRC: &str = "object Wrap {\n\
    fun withReceiver(f: Int.() -> Int): Int = 5.f()\n\
}\n\
fun box(): String {\n\
    val r = Wrap.withReceiver { this * 2 }\n\
    return if (r == 10) \"OK\" else \"FAIL: $r\"\n\
}\n";
    expect_ok(SRC, "ObjectReceiverLambdaWithReceiverParam");
}

/// A companion member with a defaulted parameter before the trailing lambda: the lambda's types
/// come from the candidate's parameter list, mapped through the defaulted slots.
#[test]
fn companion_receiver_lambda_after_default() {
    const SRC: &str = "class Holder {\n\
    companion object {\n\
        fun apply2(n: Int = 5, f: (Int) -> Int): Int = f(n)\n\
    }\n\
}\n\
fun box(): String {\n\
    val r = Holder.apply2 { it * 2 }\n\
    return if (r == 10) \"OK\" else \"FAIL: $r\"\n\
}\n";
    expect_ok(SRC, "CompanionReceiverLambdaAfterDefault");
}

#[test]
fn source_companion_invoke_with_lambda() {
    const SRC: &str = "class Wrap(val v: Int) {\n\
    companion object {\n\
        operator fun invoke(f: (Int) -> Int): Int = f(5)\n\
    }\n\
}\n\
fun box(): String {\n\
    val r = Wrap { it * 2 }\n\
    return if (r == 10) \"OK\" else \"FAIL: $r\"\n\
}\n";
    expect_ok(SRC, "SourceCompanionInvokeLambda");
}

/// An interface has no constructor, so its companion `invoke` was already selected — but the
/// arguments were typed for the (absent) construction, so the lambda still bound `it` as `Any`.
#[test]
fn source_interface_companion_invoke_with_lambda() {
    const SRC: &str = "interface Face {\n\
    companion object {\n\
        operator fun invoke(f: (Int) -> Int): Int = f(5)\n\
    }\n\
}\n\
fun box(): String {\n\
    val r = Face { it * 2 }\n\
    return if (r == 10) \"OK\" else \"FAIL: $r\"\n\
}\n";
    expect_ok(SRC, "SourceInterfaceCompanionInvokeLambda");
}

#[test]
fn source_companion_invoke_does_not_shadow_applicable_constructor() {
    const SRC: &str = "class Wrap(val v: Int) {\n\
    companion object {\n\
        operator fun invoke(f: (Int) -> Int): Int = f(5)\n\
    }\n\
}\n\
fun box(): String {\n\
    val w = Wrap(7)\n\
    return if (w.v == 7) \"OK\" else \"FAIL: ${w.v}\"\n\
}\n";
    expect_ok(SRC, "SourceCompanionInvokeCtorWins");
}

/// Selecting the companion operator must not depend on whether the argument bodies type-check.
/// An error inside the lambda is reported once, as itself — not compounded with the construction's
/// failure (`cannot create an instance of an interface`), and not with the provisional
/// `it`-as-`Any` complaint from the pass that typed the arguments for the constructor.
#[test]
fn companion_invoke_reports_only_the_lambda_body_error() {
    let Some(d) = diags(
        "interface Face {\n\
             companion object {\n\
                 operator fun invoke(f: (Int) -> Int): Face = object : Face {}\n\
             }\n\
         }\n\
         fun go(): Face = Face { it + undefinedThing() }\n",
    ) else {
        return;
    };
    assert_eq!(
        d.len(),
        1,
        "one diagnostic for the unresolved call in the lambda body, got {d:?}"
    );
    assert!(
        d[0].contains("undefinedThing"),
        "the reported diagnostic must be the lambda body's own, got {d:?}"
    );
}

/// The same for a source CLASS: the constructor is inapplicable, so the operator is selected, and
/// only the argument's own error survives.
#[test]
fn source_class_companion_invoke_reports_only_the_argument_error() {
    let Some(d) = diags(
        "class Holder {\n\
             companion object {\n\
                 operator fun invoke(f: (Int) -> Int): Int = f(5)\n\
             }\n\
         }\n\
         fun go(): Int = Holder(nope())\n",
    ) else {
        return;
    };
    assert_eq!(
        d.len(),
        1,
        "one diagnostic for the unresolved argument, got {d:?}"
    );
    assert!(
        d[0].contains("nope"),
        "the reported diagnostic must be the argument's own, got {d:?}"
    );
}

/// A companion with no `invoke` still reports the constructor's own failure — the provisional
/// argument diagnostics are dropped only when the operator is actually selected.
#[test]
fn inapplicable_constructor_without_companion_invoke_still_reports() {
    let Some(d) = diags(
        "class Holder(val v: Int)\n\
         fun go(): Holder = Holder { 1 }\n",
    ) else {
        return;
    };
    assert!(
        !d.is_empty(),
        "a lambda argument to `Holder(Int)` must be rejected"
    );
}
