//! Top-level extension functions overloaded by arity on the same receiver — `fun R.f()` and
//! `fun R.f(x)` — were wrongly rejected at signature collection ("conflicting extension functions
//! with the same erased receiver and name"). `ext_funs` now holds all overloads; each call resolves
//! (checker) and lowers (backend) to the overload matching its argument count. Same-file, JVM.
use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn arity_overloaded_extension_on_user_class() {
    // `Box.f()` delegates to `Box.f(1)`; both overloads coexist and each call binds its own arity.
    const SRC: &str = "\
class Box(val n: Int)\n\
fun Box.f() = f(1)\n\
fun Box.f(k: Int): Int = n + k\n\
fun box(): String =\n\
    if (Box(10).f() == 11 && Box(10).f(5) == 15) \"OK\" else \"FAIL\"\n";
    assert_eq!(run(SRC).expect("arity-overloaded extension"), "OK");
}

#[test]
fn arity_overloaded_extension_distinct_bodies() {
    // Three overloads by arity, each with its own body — verifies per-overload dispatch, not just
    // that two coexist.
    const SRC: &str = "\
class S(val v: String)\n\
fun S.tag() = \"none\"\n\
fun S.tag(a: String) = a\n\
fun S.tag(a: String, b: String) = a + b\n\
fun box(): String {\n\
    val s = S(\"x\")\n\
    return if (s.tag() == \"none\" && s.tag(\"O\") == \"O\" && s.tag(\"O\", \"K\") == \"OK\")\n\
        \"OK\" else \"FAIL\"\n\
}\n";
    assert_eq!(run(SRC).expect("three-arity overloaded extension"), "OK");
}
