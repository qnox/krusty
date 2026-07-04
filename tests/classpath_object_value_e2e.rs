//! A CLASSPATH Kotlin `object` referenced as a bare value (not a member call) reads `getstatic
//! <internal>.INSTANCE`. `EmptyCoroutineContext` is a public `object` in the stdlib; its `toString()`
//! is the singleton's own. Round-tripped under `-Xverify:all`.

use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "C")
}

#[test]
fn classpath_object_referenced_as_value() {
    const SRC: &str = "import kotlin.coroutines.EmptyCoroutineContext\n\
fun box(): String {\n\
    val c = EmptyCoroutineContext\n\
    return if (c.toString() == \"EmptyCoroutineContext\") \"OK\" else c.toString()\n\
}\n";
    let out = run(SRC).expect("classpath object as value should compile + run");
    assert_eq!(out, "OK");
}

#[test]
fn classpath_object_via_wildcard_import() {
    const SRC: &str = "import kotlin.coroutines.*\n\
fun box(): String {\n\
    val c = EmptyCoroutineContext\n\
    return if (c.toString() == \"EmptyCoroutineContext\") \"OK\" else c.toString()\n\
}\n";
    let out = run(SRC).expect("classpath object via wildcard import should compile + run");
    assert_eq!(out, "OK");
}
