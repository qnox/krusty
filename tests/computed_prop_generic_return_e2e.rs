//! A class-member COMPUTED property (`val x get() = <expr>`) whose getter body is a classpath member-call
//! chain returning a generic type (`Holder<String>`) must keep the concrete type argument — so a later
//! `x.member` / `x.field.method()` resolves against the real element type, not `Any`. The computed-getter
//! type inference used a limited helper that returned `Error` for a member-call chain, erasing the
//! property; every downstream access then read `Any` (the reactive-Mongo `collection.find{}.map{ it… }`
//! shape). Now it uses the full resolver-based inference (except in a value class, which keeps the
//! conservative helper — a richer type breaks its specialized member emit). Round-tripped on the JVM.

use super::common;

const LIB: &str = "package lib\n\
    class Holder<T>(val v: T)\n\
    object Make { fun str(): Holder<String> = Holder(\"hi\") }\n";

fn run(main: &str) -> Option<String> {
    let jdk = common::jdk_modules()?;
    let sl = common::stdlib_jar()?;
    let lo = common::compile_lib("computed_generic", LIB)?;
    common::compile_and_run_box(main, "Main", &[lo, sl], Some(&jdk))
}

#[test]
fn computed_property_keeps_classpath_generic_return_arg() {
    // `h` is a computed property over `Make.str(): Holder<String>`. Its element `v` must type as `String`,
    // so `h.v.length` resolves (was `Any` → "unresolved member 'length'").
    const MAIN: &str = "import lib.Make\n\
        class C { val h get() = Make.str() }\n\
        fun box(): String = if (C().h.v.length == 2) \"OK\" else \"F:\" + C().h.v.length\n";
    assert_eq!(run(MAIN).expect("computed property generic return"), "OK");
}
