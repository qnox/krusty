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

#[test]
fn leading_elvis_continues() {
    // `?:` on the next line continues the expression (it cannot begin a statement). Previously the
    // newline ended the `val` and the leading `?:` desynced the parse for the rest of the file.
    const SRC: &str = "fun g(): String? = null\n\
fun box(): String {\n  val x = g()\n    ?: \"OK\"\n  return x\n}\n";
    assert_eq!(run(SRC).expect("leading ?: continues"), "OK");
}

#[test]
fn leading_and_or_continue() {
    // `&&` / `||` leading a continuation line keep the boolean expression going.
    const SRC: &str = "fun box(): String {\n  val a = true\n  val b = false\n\
  return if (a\n      && !b\n      || b) \"OK\" else \"no\"\n}\n";
    assert_eq!(run(SRC).expect("leading &&/|| continue"), "OK");
}

#[test]
fn leading_plus_does_not_continue() {
    // `+`/`-` do NOT continue across a newline (kotlinc parses a leading `+x` as a fresh unary
    // statement). Here `val a = f()` then a discarded `+2`, so `a` stays 1.
    const SRC: &str = "fun f(): Int = 1\n\
fun box(): String {\n  val a = f()\n    + 2\n  return if (a == 1) \"OK\" else \"no\"\n}\n";
    assert_eq!(run(SRC).expect("leading + is a separate statement"), "OK");
}
