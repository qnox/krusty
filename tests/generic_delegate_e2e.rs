//! A generic delegate's `getValue` returns the erased `Object` (`<T> getValue(): T`); a delegated
//! member property of a concrete type inserts the `checkcast`/unbox kotlinc emits. Round-tripped under
//! `-Xverify:all`.

mod common;

fn run(src: &str) -> Option<String> {
    let jh = common::java_home()?;
    let sl = common::stdlib_jar()?;
    let jdk = std::path::PathBuf::from(format!("{jh}/lib/modules"));
    common::compile_and_run_box(src, "C", &[sl], Some(&jdk))
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
