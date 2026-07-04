//! `object O : Base(args)` — an object (incl. a sealed-hierarchy `object A : S()`) extending a class.
//! `parse_object` now captures the base class + super-args (previously ignored); the general class
//! lowering computes the `superclass` + emits the `super(args)` call. This also makes a sealed hierarchy
//! of objects exhaustive in a `when (s) { is A -> … }` (the objects are now registered as subclasses of
//! the sealed base). An object with BOTH a base class and interfaces (qualified `super<T>`) skips.

use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn object_extends_class_with_arg() {
    const SRC: &str = "open class Base(val n: Int)\n\
object O : Base(5)\n\
fun box(): String = if (O.n == 5) \"OK\" else \"no\"\n";
    assert_eq!(
        run(SRC).expect("object extends class compiles + runs"),
        "OK"
    );
}

#[test]
fn sealed_object_hierarchy_when_is() {
    const SRC: &str = "sealed class S\nobject A : S()\nobject B : S()\n\
fun f(s: S): String = when (s) { is A -> \"O\"; is B -> \"K\" }\n\
fun box(): String = f(A) + f(B)\n";
    assert_eq!(run(SRC).expect("sealed-object when compiles + runs"), "OK");
}

#[test]
fn object_extends_open_class_method() {
    const SRC: &str = "open class Base { open fun g(): String = \"no\" }\n\
object O : Base() { override fun g() = \"OK\" }\n\
fun box(): String { val b: Base = O; return b.g() }\n";
    assert_eq!(
        run(SRC).expect("object override via base compiles + runs"),
        "OK"
    );
}
