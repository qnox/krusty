//! A CLASSPATH Kotlin `object` referenced as a bare value (not a member call) reads `getstatic
//! <internal>.INSTANCE`. `EmptyCoroutineContext` is a public `object` in the stdlib; its `toString()`
//! is the singleton's own. Round-tripped under `-Xverify:all`.

mod common;

fn run(src: &str) -> Option<String> {
    let jh = common::java_home()?;
    let sl = common::stdlib_jar()?;
    let jdk = std::path::PathBuf::from(format!("{jh}/lib/modules"));
    common::compile_and_run_box(src, "C", &[sl], Some(&jdk))
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
