//! A constructor parameter whose default value is an OBJECT singleton (`= EmptyCoroutineContext`, a
//! classpath `object` read as `getstatic …INSTANCE`) — not a simple literal. The checker now visits
//! primary-constructor parameter defaults (recording their object-value references), so the
//! `super(<defaults>)` synthesized for a subclass / typed companion can lower a non-literal default.
//! Previously only literal defaults were modeled, so a base like the coroutine `EmptyContinuation`
//! (`context: CoroutineContext = EmptyCoroutineContext`) bailed. Round-tripped on the JVM.

use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn subclass_fills_object_singleton_base_default() {
    // `B : A()` where A's only ctor param defaults to a classpath object — the `super(…)` call fills it.
    const SRC: &str = "import kotlin.coroutines.*\n\
open class A(val ctx: CoroutineContext = EmptyCoroutineContext) { fun tag() = \"OK\" }\n\
class B : A()\n\
fun box(): String { val a: A = B(); return a.tag() }\n";
    assert_eq!(
        run(SRC).expect("subclass fills an object-singleton base default + runs"),
        "OK"
    );
}

#[test]
fn self_ref_companion_fills_object_singleton_default() {
    // The coroutine `EmptyContinuation` shape: a class whose companion extends ITSELF, the ctor param
    // defaulting to a classpath object. `C` used as a value is its companion (an instance of `C`).
    const SRC: &str = "import kotlin.coroutines.*\n\
open class C(val ctx: CoroutineContext = EmptyCoroutineContext) {\n\
  companion object : C()\n\
  fun tag() = \"OK\"\n\
}\n\
fun box(): String { val c: C = C; return c.tag() }\n";
    assert_eq!(
        run(SRC).expect("self-referential companion fills an object-singleton default + runs"),
        "OK"
    );
}
