//! A LAMBDA argument passed through the implicit-`invoke` convention (`b { it + 1 }` where `b`
//! carries an `operator fun invoke(f: (Int) -> Int)`). The lambda's parameter types come from the
//! selected `invoke`, exactly as they would for a normally-named method — without that expectation
//! `it` binds as `Any` and the body is rejected before the callable is ever selected.

use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn member_invoke_binds_a_trailing_lambdas_implicit_it() {
    const SRC: &str = "class Box(val v: Int) {\n\
    operator fun invoke(f: (Int) -> Int): Int = f(v)\n\
}\n\
fun box(): String {\n\
    val b = Box(7)\n\
    val r = b { it + 1 }\n\
    if (r != 8) return \"r=$r\"\n\
    return \"OK\"\n\
}\n";
    assert_eq!(
        run(SRC).expect("member invoke with a trailing lambda"),
        "OK"
    );
}

#[test]
fn inline_member_invoke_binds_a_trailing_lambdas_implicit_it() {
    // The `inline` form splices the lambda body into the caller; the parameter binding is the same.
    const SRC: &str = "class Box(val v: Int) {\n\
    inline operator fun invoke(f: (Int) -> Int): Int = f(v)\n\
}\n\
fun box(): String {\n\
    val b = Box(7)\n\
    val r = b { it + 1 }\n\
    if (r != 8) return \"r=$r\"\n\
    return \"OK\"\n\
}\n";
    assert_eq!(run(SRC).expect("inline member invoke with a lambda"), "OK");
}

#[test]
fn member_invoke_binds_a_named_lambda_parameter() {
    // A NAMED lambda parameter with no declared type takes the same expectation as implicit `it`.
    const SRC: &str = "class Joiner(val sep: String) {\n\
    operator fun invoke(f: (String) -> String): String = f(sep)\n\
}\n\
fun box(): String {\n\
    val j = Joiner(\"-\")\n\
    val r = j { s -> s + s.length }\n\
    if (r != \"-1\") return \"r=$r\"\n\
    return \"OK\"\n\
}\n";
    assert_eq!(
        run(SRC).expect("member invoke with a named lambda param"),
        "OK"
    );
}

#[test]
fn member_invoke_binds_a_lambda_alongside_a_value_argument() {
    // A lambda in trailing position beside an ordinary argument: only the function-typed parameter
    // takes a lambda expectation, the rest type as usual.
    const SRC: &str = "class Scaler(val v: Int) {\n\
    operator fun invoke(k: Int, f: (Int) -> Int): Int = f(v * k)\n\
}\n\
fun box(): String {\n\
    val s = Scaler(3)\n\
    val r = s(2) { it + 1 }\n\
    if (r != 7) return \"r=$r\"\n\
    return \"OK\"\n\
}\n";
    assert_eq!(run(SRC).expect("member invoke with a value + lambda"), "OK");
}

#[test]
fn invoke_on_a_returned_receiver_binds_its_lambda() {
    // The arbitrary-callee shape `make(n)( … )`: the receiver is an expression, not a name, and the
    // convention's parameter types still reach the lambda. (The lambda must be parenthesized —
    // written trailing it would attach to `make` instead.)
    const SRC: &str = "class Box(val v: Int) {\n\
    operator fun invoke(f: (Int) -> Int): Int = f(v)\n\
}\n\
fun make(n: Int): Box = Box(n)\n\
fun box(): String {\n\
    val r = make(7)({ it + 1 })\n\
    if (r != 8) return \"r=$r\"\n\
    return \"OK\"\n\
}\n";
    assert_eq!(
        run(SRC).expect("invoke on a call-expression receiver"),
        "OK"
    );
}
