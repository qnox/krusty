//! Cast to a nullable reference type (`x as Foo?`). This is a plain `checkcast Foo` — `null` passes
//! through the checkcast (so `null as Foo?` is `null`, never a throw), a wrong non-null type throws
//! `ClassCastException`, and a matching value casts. (Contrast `x as Foo`, which null-checks first.)
//! Round-tripped on the JVM.

mod common;

#[test]
fn nullable_reference_cast_passes_null_and_checkcasts() {
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
    common::expect_box_ok_with_stdlib(SRC, "P");
}
