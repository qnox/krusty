//! A `when` used as a STATEMENT whose branches mix a `Unit` body (an assignment) with a diverging
//! `Nothing` body (`else -> throw`). The flat-IR lowerer rejects `Unit` mixed with a *real* value (it
//! can't distinguish a discarded statement from a value use → inconsistent frames), but a `Nothing`
//! branch pushes nothing at the merge, so it is exempt. Round-tripped on the JVM.

mod common;

fn run(src: &str) -> Option<String> {
    let sl = common::stdlib_jar()?;
    let jdk = common::jdk_modules()?;
    common::compile_and_run_box(src, "Main", &[sl], Some(&jdk))
}

fn ready() -> bool {
    common::java_home().is_some() && common::stdlib_jar().is_some()
}

#[test]
fn when_statement_with_throwing_else() {
    if !ready() {
        return;
    }
    // `is` subject smart-cast, assignment in the match arm, `throw` in the `else`.
    const SRC: &str = "class A { fun foo(x: Int = 32): String = \"OK\" }\n\
var result = \"failed\"\n\
fun whoops(x: Any) {\n\
    when (x) {\n\
        is A -> result = x.foo()\n\
        else -> throw AssertionError()\n\
    }\n\
}\n\
fun box(): String {\n\
    whoops(A())\n\
    return result\n\
}\n";
    assert_eq!(
        run(SRC).expect("when stmt with throwing else compiles + runs"),
        "OK"
    );
}

#[test]
fn when_value_subject_with_throwing_else() {
    if !ready() {
        return;
    }
    // A value-subject `when` statement: matched arm assigns, `else` diverges.
    const SRC: &str = "var r = \"f\"\n\
fun w(x: Int) { when (x) { 1 -> r = \"a\"; 2 -> r = \"b\"; else -> throw AssertionError() } }\n\
fun box(): String { w(1); val a = r; w(2); return if (a == \"a\" && r == \"b\") \"OK\" else \"fail\" }\n";
    assert_eq!(
        run(SRC).expect("value-subject when with throwing else compiles + runs"),
        "OK"
    );
}
