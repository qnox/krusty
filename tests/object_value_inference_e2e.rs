//! A property/local initialized from a classpath/stdlib `object` used as a value (`val ctx =
//! EmptyCoroutineContext`) infers the object's own type — no explicit annotation needed, matching
//! kotlinc. Only an `object` is a value, so a plain class name is never mistyped. Round-tripped on the JVM.

mod common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn member_property_inferred_from_classpath_object() {
    const SRC: &str = "import kotlin.coroutines.*\n\
class C { val ctx = EmptyCoroutineContext }\n\
fun box(): String = if (C().ctx === EmptyCoroutineContext) \"OK\" else \"no\"\n";
    assert_eq!(
        run(SRC).expect("object-valued member property infers + runs"),
        "OK"
    );
}
