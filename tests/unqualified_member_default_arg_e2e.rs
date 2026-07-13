//! An UNQUALIFIED call to a sibling member method that OMITS a default argument (`foo(a)` inside the
//! class, where `fun foo(a, b = …)`) must fill the default via the method's `$default` stub — exactly as
//! the qualified `this.foo(a)` form does. The unqualified path used a strict arity check and bailed
//! (skipping the whole file) on any omitted default. Round-tripped on the JVM.

use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn unqualified_sibling_call_omitting_default() {
    // `a()` calls the sibling `r(id)` omitting `o` (default `null`); `b()` supplies both.
    const SRC: &str = "class C {\n\
    fun r(id: String, o: String? = null): String = \"$id/${o ?: \"def\"}\"\n\
    fun a(id: String): String = r(id)\n\
    fun b(): String = r(\"x\", \"y\")\n\
}\n\
fun box(): String {\n\
    val c = C()\n\
    return if (c.a(\"p\") == \"p/def\" && c.b() == \"x/y\") \"OK\" else \"FAIL ${c.a(\"p\")} ${c.b()}\"\n\
}\n";
    assert_eq!(
        run(SRC).expect("omitted-default sibling call compiles + runs"),
        "OK"
    );
}
