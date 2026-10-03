//! A private function called from a non-private `inline` function.
//!
//! kotlinc rewrites that call to a public `access$` accessor — a facade `access$bar()` /
//! `access$dex()`, and an instance `access$bi(Owner)` — while the private method stays private.
//! The inline function's own method and every same-module expansion of it call that accessor.
//! A direct `invokestatic` / `invokevirtual` of the private method from the caller's class is an
//! `IllegalAccessError`.

use super::common;

const LIB: &str = "internal inline fun foo(): String = bar()\n\
\n\
private fun bar(): String = \"11\"\n\
\n\
class C {\n\
\x20   internal inline fun fi(): String = bi()\n\
\n\
\x20   private fun bi(): String = \"22\"\n\
}\n\
\n\
private fun dex(): String = \"33\"\n\
\n\
class CC {\n\
\x20   internal inline fun fx(): String = dex()\n\
}\n";

const MAIN: &str = "fun test1(): String = foo()\n\
\n\
fun test2(): String = C().fi()\n\
\n\
fun test3(): String = CC().fx()\n\
\n\
fun box(): String {\n\
\x20   if (test1() != \"11\") return \"FAIL 1\"\n\
\x20   if (test2() != \"22\") return \"FAIL 2\"\n\
\x20   if (test3() != \"33\") return \"FAIL 3\"\n\
\x20   return \"OK\"\n\
}\n";

const SOURCES: [(&str, &str); 2] = [("f1.kt", LIB), ("f2.kt", MAIN)];

#[test]
fn a_private_call_inside_an_inline_function_uses_access() {
    let facade = common::ModuleClassPair::compile(&SOURCES, "F1Kt");
    for method in ["foo", "access$bar", "access$dex"] {
        let (reference, krusty) = facade.method_code("F1Kt", method);
        assert_eq!(krusty, reference, "F1Kt.{method}");
    }
    let class = common::ModuleClassPair::compile(&SOURCES, "C");
    let (reference, krusty) = class.method_code("C", "access$bi");
    assert_eq!(krusty, reference, "C.access$bi");
    let caller = common::ModuleClassPair::compile(&SOURCES, "F2Kt");
    for method in ["test1", "test2", "test3"] {
        let (reference, krusty) = caller.method_code("F2Kt", method);
        assert_eq!(krusty, reference, "F2Kt.{method}");
    }

    let jdk = common::jdk_modules();
    let result =
        common::compile_and_run_box_files(&SOURCES, &[common::stdlib_jar()], Some(jdk.as_path()))
            .expect("krusty compiles and runs the fixture");
    assert_eq!(result, common::kotlinc_box_files_result(&SOURCES, "F2Kt"));
    assert_eq!(result, "OK");
}

/// A public `inline` function is a public API boundary. Calling a private function from it is
/// rejected at the reference; the backend does not publish an accessor for that program.
#[test]
fn a_public_inline_function_cannot_call_a_non_public_function() {
    const SOURCE: &str = "inline fun foo(): String = bar()\n\
\n\
private fun bar(): String = \"11\"\n\
\n\
class C {\n\
\x20   inline fun fi(): String = bi()\n\
\n\
\x20   private fun bi(): String = \"22\"\n\
}\n\
\n\
private fun dex(): String = \"33\"\n\
\n\
class CC {\n\
\x20   inline fun fx(): String = dex()\n\
}\n\
\n\
inline fun outer(): String = inner()\n\
\n\
private inline fun inner(): String = bar()\n";
    let result = common::compiler_diagnostics(&[("f1.kt", SOURCE)], &[]);
    common::expect_identical_rejection(
        &result,
        "a public inline function calling a non-public function",
    );
}

/// `@PublishedApi internal` is public API for an inline call. A public inline function may call
/// it. An `@PublishedApi internal inline` function is itself that boundary and cannot call a
/// private function.
#[test]
fn published_api_internal_is_public_api_for_inline_access() {
    const SOURCE: &str = "@PublishedApi\n\
internal fun publishedTop(): String = \"T\"\n\
\n\
class A {\n\
\x20   @PublishedApi\n\
\x20   internal fun publishedMember(): String = \"M\"\n\
\n\
\x20   inline fun readMember(): String = publishedMember()\n\
}\n\
\n\
inline fun readTop(): String = publishedTop()\n\
\n\
fun box(): String {\n\
\x20   if (readTop() != \"T\") return \"FAIL top\"\n\
\x20   if (A().readMember() != \"M\") return \"FAIL member\"\n\
\x20   return \"OK\"\n\
}\n";
    let sources = [("main.kt", SOURCE)];
    let jdk = common::jdk_modules();
    let result =
        common::compile_and_run_box_files(&sources, &[common::stdlib_jar()], Some(jdk.as_path()))
            .expect("krusty accepts @PublishedApi from a public inline function");
    assert_eq!(result, common::kotlinc_box_files_result(&sources, "MainKt"));
    assert_eq!(result, "OK");

    const REJECTED: &str = "@PublishedApi\n\
internal inline fun boundary(): String = hidden()\n\
\n\
private fun hidden(): String = \"NO\"\n\
\n\
class C {\n\
\x20   @PublishedApi\n\
\x20   internal inline fun callsSecret(): String = secret()\n\
\n\
\x20   private fun secret(): String = \"NO\"\n\
}\n";
    let rejected = common::compiler_diagnostics(&[("f1.kt", REJECTED)], &[]);
    common::expect_identical_rejection(
        &rejected,
        "@PublishedApi internal inline calling a private function",
    );
}

/// A private top-level function with a default, called from an internal inline function, is
/// reached through `access$foo$default` with the `$default` stub's descriptor.
#[test]
fn a_private_top_level_default_uses_the_default_bridge_accessor() {
    const LIB: &str = "private fun foo(x: Int = 1): Int = x\n\
\n\
private fun bar(x: String = \"A\", y: String = \"B\"): String = x + y\n\
\n\
internal inline fun useFoo(): Int = foo()\n\
\n\
internal inline fun useBar(): String = bar(\"Z\")\n";
    const MAIN: &str = "fun box(): String {\n\
\x20   if (useFoo() != 1) return \"FAIL foo\"\n\
\x20   if (useBar() != \"ZB\") return \"FAIL bar\"\n\
\x20   return \"OK\"\n\
}\n";
    let sources = [("f1.kt", LIB), ("f2.kt", MAIN)];
    let facade = common::ModuleClassPair::compile(&sources, "F1Kt");
    for method in [
        "access$foo$default",
        "access$bar$default",
        "useFoo",
        "useBar",
    ] {
        let (reference, krusty) = facade.method_code("F1Kt", method);
        assert_eq!(krusty, reference, "F1Kt.{method}");
    }
    let caller = common::ModuleClassPair::compile(&sources, "F2Kt");
    let (reference, krusty) = caller.method_code("F2Kt", "box");
    assert_eq!(krusty, reference, "F2Kt.box");
    let jdk = common::jdk_modules();
    let result =
        common::compile_and_run_box_files(&sources, &[common::stdlib_jar()], Some(jdk.as_path()))
            .expect("krusty runs the top-level default accessor");
    assert_eq!(result, common::kotlinc_box_files_result(&sources, "F2Kt"));
    assert_eq!(result, "OK");
}

/// A private member with a default, called from an internal inline function, is reached through
/// `access$mem$default` with the instance `$default` stub's receiver, mask, and marker.
#[test]
fn a_private_member_default_uses_the_default_bridge_accessor() {
    const LIB: &str = "class C {\n\
\x20   private fun mem(z: Int = 3): Int = z\n\
\n\
\x20   internal inline fun useMem(): Int = mem()\n\
}\n";
    const MAIN: &str = "fun box(): String {\n\
\x20   if (C().useMem() != 3) return \"FAIL\"\n\
\x20   return \"OK\"\n\
}\n";
    let sources = [("f1.kt", LIB), ("f2.kt", MAIN)];
    let class = common::ModuleClassPair::compile(&sources, "C");
    let (reference, krusty) = class.method_code("C", "access$mem$default");
    assert_eq!(krusty, reference, "C.access$mem$default");
    let caller = common::ModuleClassPair::compile(&sources, "F2Kt");
    let (reference, krusty) = caller.method_code("F2Kt", "box");
    assert_eq!(krusty, reference, "F2Kt.box");
    let jdk = common::jdk_modules();
    let result =
        common::compile_and_run_box_files(&sources, &[common::stdlib_jar()], Some(jdk.as_path()))
            .expect("krusty runs the member default accessor");
    assert_eq!(result, common::kotlinc_box_files_result(&sources, "F2Kt"));
    assert_eq!(result, "OK");
}
