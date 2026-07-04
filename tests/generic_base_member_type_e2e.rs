//! A subclass of a generic base whose member signature references the BASE's type parameter
//! (`abstract class A<T> { abstract fun f(): T }`, `class C : A<String>()`). Collecting the subclass's
//! signatures walked the base's methods but resolved their return type under the SUBCLASS's type
//! parameters (empty for `C`), so the base's `T` came back "unresolved reference 'T'". It now resolves
//! under the base's own (erased) type parameters. Round-tripped on the JVM (the override's covariant
//! return drives a bridge to the erased base signature).

use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn abstract_generic_method_return() {
    const SRC: &str = "abstract class A<T> { abstract fun f(): T }\n\
class C : A<String>() { override fun f() = \"OK\" }\n\
fun box(): String = C().f()\n";
    assert_eq!(
        run(SRC).expect("abstract generic method override compiles + runs"),
        "OK"
    );
}

#[test]
fn abstract_generic_property_through_base_method() {
    // kt2480: a non-abstract base method returns an abstract property of type `T`.
    const SRC: &str = "abstract class A<T> { abstract val some: T\n fun p(): T = some }\n\
class C : A<String>() { override val some: String get() = \"OK\" }\n\
fun box(): String = C().p()\n";
    assert_eq!(
        run(SRC).expect("abstract generic property through base method compiles + runs"),
        "OK"
    );
}
