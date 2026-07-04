//! `x as T` where `T` is a CLASS-level type parameter (`class C<T>`), used in a property initializer
//! or a member method body. The cast erases to a `checkcast` against `T`'s bound (`Object` for an
//! unbounded `T`) — an unchecked cast, exactly like kotlinc. Previously the lowerer only had TOP-LEVEL
//! function type params in scope, so a class type-param cast bailed and the whole file was skipped.
//! Round-tripped on the JVM.

use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn class_tparam_cast_in_property_init() {
    // `var storage: TV = makeAny() as TV` in a generic class body — the initializer runs in the ctor
    // with the class type param `TV` in scope as an (erased) cast target.
    const SRC: &str = "class Buildee<TV> {\n\
    var storage: TV = make() as TV\n\
    private fun make(): Any = \"OK\"\n\
}\n\
fun box(): String = Buildee<String>().storage\n";
    assert_eq!(run(SRC).expect("class tparam cast in init"), "OK");
}

#[test]
fn class_tparam_cast_in_method() {
    // `x as T` inside a member method of a generic class.
    const SRC: &str = "class Box<T> {\n\
    fun wrap(x: Any): T = x as T\n\
}\n\
fun box(): String {\n\
    val b = Box<String>()\n\
    return b.wrap(\"OK\")\n\
}\n";
    assert_eq!(run(SRC).expect("class tparam cast in method"), "OK");
}
