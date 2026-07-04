//! A `when`-STATEMENT whose arms assign (Unit) plus an arm that calls a `Nothing`-returning function is
//! valid: the `Nothing` arm pushes nothing at the merge (the emitter terminates it). The checker
//! represents that arm's type as the mapped `Void` object, so the lowerer's "mixes Unit with a value"
//! bail must recognize it as diverging. Round-tripped on the JVM.

mod common;

fn run(src: &str) -> Option<String> {
    let sl = common::stdlib_jar()?;
    let jdk = common::jdk_modules()?;
    common::compile_and_run_box(src, "Main", &[sl], Some(&jdk))
}

#[test]
fn when_statement_with_nothing_arm() {
    const SRC: &str = "fun fail(): Nothing = throw RuntimeException(\"x\")\n\
fun pick(n: Int): String {\n\
    var s = \"\"\n\
    when (n) {\n\
        1 -> s = \"one\"\n\
        2 -> s = \"two\"\n\
        else -> fail()\n\
    }\n\
    return s\n\
}\n\
fun box(): String = if (pick(1) == \"one\" && pick(2) == \"two\") \"OK\" else \"fail\"\n";
    assert_eq!(run(SRC).expect("when with Nothing arm"), "OK");
}
