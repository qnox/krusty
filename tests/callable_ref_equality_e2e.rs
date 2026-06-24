//! Callable references are emitted as `kotlin/jvm/internal/FunctionReferenceImpl` subclasses (not bare
//! `LambdaMetafactory` closures), so they carry real Kotlin reference EQUALITY: two references to the
//! same top-level function are equal; a bound member reference equals another with the SAME receiver but
//! differs from one with a different receiver and from the unbound reference. Round-tripped under
//! `-Xverify:all`.

mod common;

fn run(src: &str) -> Option<String> {
    let jh = common::java_home()?;
    let sl = common::stdlib_jar()?;
    let jdk = std::path::PathBuf::from(format!("{jh}/lib/modules"));
    common::compile_and_run_box(src, "C", &[sl], Some(&jdk))
}

#[test]
fn function_reference_equality() {
    const SRC: &str = "fun top1(p: String) {}\n\
fun top2(p: String) {}\n\
class Foo { fun mem(p: String) {} }\n\
fun ckEq(x: Any, y: Any) { if (x != y || x.hashCode() != y.hashCode()) throw AssertionError(\"$x != $y\") }\n\
fun ckNe(x: Any, y: Any) { if (x == y) throw AssertionError(\"$x == $y\") }\n\
fun box(): String {\n\
    ckEq(::top1, ::top1)\n\
    ckNe(::top1, ::top2)\n\
    val foo = Foo()\n\
    val bar = Foo()\n\
    ckEq(foo::mem, foo::mem)\n\
    ckNe(foo::mem, bar::mem)\n\
    ckNe(foo::mem, Foo::mem)\n\
    return \"OK\"\n\
}\n";
    let out = run(SRC).expect("function reference equality should hold");
    assert_eq!(out, "OK");
}

#[test]
fn function_reference_still_invokes() {
    // The FunctionReferenceImpl subclass must still work as a Function in a higher-order call, with a
    // value-returning target, an unbound member, and a bound member.
    const SRC: &str = "fun twice(x: Int) = x * 2\n\
class Acc(val base: Int) { fun add(x: Int) = base + x }\n\
fun ap(f: (Int) -> Int, v: Int) = f(v)\n\
fun ap2(f: (Acc, Int) -> Int, a: Acc, v: Int) = f(a, v)\n\
fun box(): String {\n\
    if (ap(::twice, 5) != 10) return \"fail top\"\n\
    val acc = Acc(100)\n\
    if (ap(acc::add, 7) != 107) return \"fail bound\"\n\
    if (ap2(Acc::add, Acc(1), 2) != 3) return \"fail unbound\"\n\
    return \"OK\"\n\
}\n";
    let out = run(SRC).expect("function references should still invoke");
    assert_eq!(out, "OK");
}
