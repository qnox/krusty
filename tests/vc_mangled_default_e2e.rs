//! A top-level function with a VALUE-CLASS parameter is name-mangled (`foo-<hash>`); when it also
//! has defaulted parameters, kotlinc emits the matching mangled `foo-<hash>$default(erased params…,
//! int mask, Object marker)` synthetic, and an omitted-argument call routes through it. Mirrors
//! corpus `inlineClasses/mangledDefaultParameterFunction.kt`.

use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn vc_param_with_lambda_default_omitted() {
    const SRC: &str = "@JvmInline\n\
value class X(val s: String)\n\
fun foo(x: X, block: (X) -> String = { it.s }) = block(x)\n\
fun box(): String {\n\
    return foo(X(\"OK\"))\n\
}\n";
    assert_eq!(run(SRC).expect("vc param lambda default omitted"), "OK");
}

#[test]
fn vc_param_with_lambda_default_provided() {
    const SRC: &str = "@JvmInline\n\
value class X(val s: String)\n\
fun foo(x: X, block: (X) -> String = { it.s }) = block(x)\n\
fun box(): String {\n\
    return foo(X(\"ok\")) { it.s.uppercase() }\n\
}\n";
    assert_eq!(run(SRC).expect("vc param lambda default provided"), "OK");
}

#[test]
fn vc_param_with_non_const_default_omitted() {
    // A non-lambda, non-const default on a mangled function — same `foo-<hash>$default` routing.
    const SRC: &str = "@JvmInline\n\
value class X(val s: String)\n\
fun mk(): String = \"K\"\n\
fun foo(x: X, tail: String = mk()) = x.s + tail\n\
fun box(): String {\n\
    return foo(X(\"O\"))\n\
}\n";
    assert_eq!(run(SRC).expect("vc param non-const default omitted"), "OK");
}

#[test]
fn vc_int_underlying_param_default_omitted() {
    // A PRIMITIVE-underlying value-class parameter erases to a primitive slot; the omitted-slot
    // placeholder in the `$default` call must be the primitive zero, not null.
    const SRC: &str = "@JvmInline\n\
value class N(val v: Int)\n\
fun mk(): Int = 40\n\
fun foo(n: N, extra: Int = mk()) = n.v + extra\n\
fun box(): String {\n\
    val r = foo(N(2))\n\
    return if (r == 42) \"OK\" else \"FAIL: $r\"\n\
}\n";
    assert_eq!(run(SRC).expect("vc int underlying default omitted"), "OK");
}

#[test]
fn vc_lambda_through_inline_hof_still_splices() {
    // A VC-signature lambda passed to a SAME-FILE inline HOF splices via `inline_body`, whose nodes
    // the boxed-own-param unbox rewrite must NOT corrupt (the spliced frame's slot numbering differs
    // from the standalone impl's — an injected `unbox-impl` there would unbox an already-unboxed
    // value).
    const SRC: &str = "@JvmInline\n\
value class X(val s: String)\n\
inline fun use(x: X, f: (X) -> String): String = f(x)\n\
fun box(): String {\n\
    return use(X(\"OK\")) { it.s }\n\
}\n";
    assert_eq!(run(SRC).expect("vc lambda through inline hof"), "OK");
}

#[test]
fn vc_defaulted_param_itself_omitted() {
    // The VALUE-CLASS parameter itself carries the default — the stub fills a value-class-typed
    // slot (erased underlying) from the default expression.
    const SRC: &str = "@JvmInline\n\
value class X(val s: String)\n\
fun mk(): X = X(\"OK\")\n\
fun foo(x: X = mk()) = x.s\n\
fun box(): String {\n\
    return foo()\n\
}\n";
    assert_eq!(run(SRC).expect("vc defaulted param omitted"), "OK");
}
