//! Cast to a nullable reference type (`x as Foo?`). This is a plain `checkcast Foo` — `null` passes
//! through the checkcast (so `null as Foo?` is `null`, never a throw), a wrong non-null type throws
//! `ClassCastException`, and a matching value casts. (Contrast `x as Foo`, which null-checks first.)
//! Round-tripped on the JVM.

mod common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "P")
}

/// Skip (not fail) when the JVM + stdlib jar this e2e needs is absent.
fn toolchain_ready() -> bool {
    common::java_home().is_some() && common::stdlib_jar().is_some()
}

#[test]
fn nullable_reference_cast_passes_null_and_checkcasts() {
    if !toolchain_ready() {
        return;
    }
    const SRC: &str = "// WITH_STDLIB\n\
class Foo(val v: Int)\n\
fun box(): String {\n\
    val a: Any? = Foo(7)\n\
    val f = a as Foo?\n\
    if (f?.v != 7) return \"fail cast\"\n\
    val n: Any? = null\n\
    if ((n as Foo?) != null) return \"fail null\"\n\
    var r = \"fail cce\"\n\
    try { val bad: Any? = \"x\"; bad as Foo? } catch (e: ClassCastException) { r = \"OK\" }\n\
    return r\n\
}\n";
    assert_eq!(run(SRC).expect("`as Foo?` should compile + run"), "OK");
}
