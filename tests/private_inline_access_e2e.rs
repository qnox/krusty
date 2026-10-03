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
