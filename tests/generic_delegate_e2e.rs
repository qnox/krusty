//! A generic delegate's `getValue` returns the erased `Object` (`<T> getValue(): T`); a delegated
//! member property of a concrete type inserts the `checkcast`/unbox kotlinc emits. Round-tripped under
//! `-Xverify:all`.

use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "C")
}

#[test]
fn generic_delegate_reference_property() {
    const SRC: &str = "import kotlin.reflect.KProperty\n\
class Del<T>(val v: T) { operator fun getValue(t: Any?, p: KProperty<*>): T = v }\n\
class C { val s: String by Del(\"OK\") }\n\
fun box(): String = C().s\n";
    let out = run(SRC).expect("generic delegate should compile + run");
    assert_eq!(out, "OK");
}

#[test]
fn generic_delegate_var_primitive_property() {
    // A `var Int by Delegate<Int>` boxes the value into `setValue`'s erased param and unboxes the
    // `getValue` result — both coercions kotlinc emits.
    const SRC: &str = "import kotlin.reflect.KProperty\n\
class Del<T>(var inner: T) {\n\
    operator fun getValue(t: Any?, p: KProperty<*>): T = inner\n\
    operator fun setValue(t: Any?, p: KProperty<*>, i: T) { inner = i }\n\
}\n\
class C { var n: Int by Del(1) }\n\
fun box(): String {\n\
    val c = C()\n\
    if (c.n != 1) return \"fail get\"\n\
    c.n = 2\n\
    return if (c.n == 2) \"OK\" else \"fail set\"\n\
}\n";
    let out = run(SRC).expect("var generic delegate should compile + run");
    assert_eq!(out, "OK");
}
