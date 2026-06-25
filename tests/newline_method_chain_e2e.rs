//! A postfix method chain may continue on the next line: Kotlin treats a newline before `.` (or `?.`)
//! as part of the selector chain, not a statement terminator. `parse_postfix` previously broke at the
//! newline, so `B()\n  .a()\n  .r()` failed with "expected an expression" at the leading `.`. It now
//! peeks past the newline(s); a following `.`/`?.` continues the chain. Round-tripped on the JVM.

mod common;

fn run(src: &str) -> Option<String> {
    let jh = common::java_home()?;
    let sl = common::stdlib_jar()?;
    let jdk = std::path::PathBuf::from(format!("{jh}/lib/modules"));
    common::compile_and_run_box(src, "Main", &[sl], Some(&jdk))
}

#[test]
fn leading_dot_chain() {
    const SRC: &str = "class B { fun a(): B = this\n fun r(): String = \"OK\" }\n\
fun box(): String = B()\n    .a()\n    .a()\n    .r()\n";
    assert_eq!(run(SRC).expect("leading-dot chain compiles + runs"), "OK");
}

#[test]
fn newline_does_not_continue_non_chain() {
    // A newline NOT followed by `.` still ends the statement (two separate statements here).
    const SRC: &str = "fun box(): String {\n  val a = \"O\"\n  val b = \"K\"\n  return a + b\n}\n";
    assert_eq!(run(SRC).expect("non-chain newline unaffected"), "OK");
}
