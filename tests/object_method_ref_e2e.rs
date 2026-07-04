//! An object/singleton method reference `O::m` is a BOUND callable reference: it captures the
//! singleton `O.INSTANCE` and its arity is the method's own parameters (the receiver is not a
//! parameter). Lowered to a closure capturing `getstatic O.INSTANCE`. Round-tripped under
//! `-Xverify:all`.

use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "C")
}

#[test]
fn object_method_reference_is_bound_to_singleton() {
    const SRC: &str = "object Doubler {\n\
    val base = 100\n\
    fun dbl(x: Int) = x * 2 + base\n\
    fun add(a: Int, b: Int) = a + b + base\n\
}\n\
fun apply1(f: (Int) -> Int, v: Int) = f(v)\n\
fun apply2(f: (Int, Int) -> Int, a: Int, b: Int) = f(a, b)\n\
fun box(): String {\n\
    if (apply1(Doubler::dbl, 3) != 106) return \"fail dbl: \" + apply1(Doubler::dbl, 3)\n\
    if (apply2(Doubler::add, 4, 5) != 109) return \"fail add\"\n\
    val g = Doubler::dbl\n\
    if (g(10) != 120) return \"fail g\"\n\
    return \"OK\"\n\
}\n";
    let out = run(SRC).expect("object method reference should compile + run");
    assert_eq!(out, "OK");
}
