//! Callable references to EXTENSION functions: unbound `Type::ext` and bound `obj::ext`. Both lower to
//! a `FunctionReferenceImpl` calling the lifted static extension — unbound via `Static` (receiver is the
//! first invoke param), bound via `StaticBound` (receiver captured, passed as the first static arg).
//! Both carry real reference EQUALITY. Round-tripped on the JVM.

use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn unbound_extension_reference_call() {
    const SRC: &str = "class A { var result = \"Fail\" }\n\
fun A.foo() { result = \"OK\" }\n\
fun box(): String {\n\
    val a = A()\n\
    val x = A::foo\n\
    x(a)\n\
    return a.result\n\
}\n";
    assert_eq!(run(SRC).expect("unbound A::ext call"), "OK");
}

#[test]
fn bound_extension_reference_call() {
    const SRC: &str = "class A(val v: String)\n\
fun A.foo(suffix: String): String = v + suffix\n\
fun box(): String {\n\
    val a = A(\"O\")\n\
    val g = a::foo\n\
    return g(\"K\")\n\
}\n";
    assert_eq!(run(SRC).expect("bound obj::ext call"), "OK");
}

#[test]
fn extension_reference_equality() {
    // Unbound refs to the same extension are equal; bound refs on the same receiver are equal; a bound
    // and an unbound ref are NOT equal.
    const SRC: &str = "class Foo\n\
fun Foo.ext(): Unit {}\n\
fun box(): String {\n\
    val foo = Foo()\n\
    if (Foo::ext != Foo::ext) return \"f1\"\n\
    if (foo::ext != foo::ext) return \"f2\"\n\
    if (foo::ext == Foo::ext) return \"f3\"\n\
    return \"OK\"\n\
}\n";
    assert_eq!(run(SRC).expect("ext ref equality"), "OK");
}
