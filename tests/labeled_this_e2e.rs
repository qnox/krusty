//! Labeled `this` (`this@C`). The parser now accepts `this@Label` / `super@Label` (previously
//! "expected an expression"). A SELF-label — `this@C` inside `C`'s own member, often via a lambda
//! (`run { this@C.bar() }`) — resolves to the current `this`. Outer-class / receiver-lambda / accessor
//! labels need a receiver-label stack krusty does not track yet (those files skip, never miscompile).

mod common;

fn run(src: &str) -> Option<String> {
    let jh = common::java_home()?;
    let sl = common::stdlib_jar()?;
    let jdk = std::path::PathBuf::from(format!("{jh}/lib/modules"));
    common::compile_and_run_box(src, "Main", &[sl], Some(&jdk))
}

fn ready() -> bool {
    common::java_home().is_some() && common::stdlib_jar().is_some()
}

#[test]
fn self_labeled_this_in_lambda() {
    if !ready() {
        return;
    }
    // `this@C` inside a lambda in C's own method resolves to C's receiver.
    const SRC: &str = "class C(val v: String) {\n\
    fun foo(): String = run { this@C.bar() }\n\
    fun bar(): String = v\n\
}\n\
fun box(): String = C(\"OK\").foo()\n";
    assert_eq!(run(SRC).expect("self-labeled this compiles + runs"), "OK");
}
