//! A user top-level generic function whose return is a type parameter — `fun <T> id(x: T) = x` — must
//! recover its concrete return type at a call site so member/operator resolution works: `id("hi").length`
//! resolves `length` on `String` instead of erasing to `Any`. The return type parameter is read from an
//! explicit `: T` annotation OR (when the return is inferred) an expression body that is a bare parameter
//! reference. A non-inline function's return crosses the JVM erasure boundary, so a primitive binding is
//! typed as its boxed wrapper; a reference binding stays as itself. Same-file, runnable.
use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn generic_identity_reference_return_member() {
    // Inferred return (`= x`) bound to a reference type: members of the result resolve.
    const SRC: &str = "fun <T> id(x: T) = x\n\
        fun box(): String {\n\
        \x20 val n = id(\"hello\").length\n\
        \x20 val u = id(\"abc\").uppercase()\n\
        \x20 return if (n == 5 && u == \"ABC\") \"OK\" else \"no:\" + n + u\n\
        }\n";
    assert_eq!(run(SRC).expect("generic identity reference return"), "OK");
}

#[test]
fn generic_identity_explicit_return_annotation() {
    // Explicit `: T` annotation, reference binding.
    const SRC: &str = "fun <T> pick(a: T, b: T): T = a\n\
        fun box(): String {\n\
        \x20 val s = pick(\"x\", \"y\")\n\
        \x20 return if (s.length == 1) \"OK\" else \"no\"\n\
        }\n";
    assert_eq!(run(SRC).expect("generic explicit return"), "OK");
}

#[test]
fn conflicting_bindings_do_not_miscompile() {
    // Two parameters binding the same return type parameter to potentially-different types (a nullable
    // arg and a plain arg) must NOT be force-inferred to one arg's type — the return inference declines
    // rather than emitting a wrong `checkcast` (a ClassCastException). The call still runs.
    const SRC: &str = "fun <T> select(x: T?, y: T): T = y\n\
        open class A\n\
        class B : A()\n\
        fun box(): String {\n\
        \x20 val r: A = select(null, B())\n\
        \x20 return if (r is B) \"OK\" else \"no\"\n\
        }\n";
    assert_eq!(run(SRC).expect("conflicting bindings run"), "OK");
}

#[test]
fn zero_arg_lambda_return_binds_type_param() {
    // A zero-parameter function-type parameter (`action: () -> T`) has an EMPTY `fun_params` list, so
    // the function-type test must not rely on non-emptiness: `T` binds from the lambda's return type
    // (KT-42130 shape). A primitive binding is typed as its boxed wrapper (`Boolean?`), so the result
    // is consumed through the boxed comparison — never as a raw erased `Object` in a branch (the
    // VerifyError this guards against). A reference binding resolves members on the result.
    const SRC: &str = "fun <T> run2(action: () -> T): T = action()\n\
        fun box(): String {\n\
        \x20 val b = run2 { true }\n\
        \x20 val s = run2 { \"ab\" }\n\
        \x20 return if (b == true && s.length == 2) \"OK\" else \"no\"\n\
        }\n";
    assert_eq!(run(SRC).expect("zero-arg lambda return binding"), "OK");
}
