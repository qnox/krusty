//! A generic instance method with its own type parameter and a function parameter
//! (`class Box<T> { fun <R> map(f: (T) -> R): R }`) must substitute types at the call site: the lambda
//! parameter `it` types as the receiver's element type `T` (`Box<String>` → `it: String`), and the
//! method type parameter `R` is inferred from the lambda body. A front-end (checker) regression guard —
//! no classpath needed (the member resolved on `it` is a builtin), so it runs everywhere.

use krusty::diag::DiagSink;
use krusty::lexer::lex;
use krusty::parser::parse;
use krusty::resolve::{check_file, collect_signatures};

fn errors(src: &str) -> Vec<String> {
    let mut d = DiagSink::new();
    let toks = lex(src, &mut d);
    let files = vec![parse(src, &toks, &mut d)];
    let mut syms = collect_signatures(&files, &mut d);
    check_file(&files[0], &mut syms, &mut d);
    d.diags.iter().map(|x| x.msg.clone()).collect()
}

#[test]
fn lambda_param_substitutes_receiver_type_arg() {
    // `it` must type as `String` (the receiver's `T`), so `it.length` resolves — not `'T'`/`'Any'`.
    let src = "class Box<T>(val v: T) { fun <R> map(f: (T) -> R): R = f(v) }\n\
fun main() { val b: Box<String> = Box(\"hi\"); val n: Int = b.map { it.length }; println(n) }\n";
    let es = errors(src);
    assert!(es.is_empty(), "expected no diagnostics, got: {es:?}");
}

#[test]
fn lambda_param_substitutes_primitive_receiver_type_arg() {
    // A primitive element type (`Box<Int>`): `it` types as `Int`, so `it * 2` resolves.
    let src = "class Box<T>(val v: T) { fun <R> map(f: (T) -> R): R = f(v) }\n\
fun main() { val b: Box<Int> = Box(21); val d: Int = b.map { it * 2 }; println(d) }\n";
    let es = errors(src);
    assert!(es.is_empty(), "expected no diagnostics, got: {es:?}");
}

#[test]
fn method_type_param_inferred_as_call_result() {
    // `R` is inferred from the lambda body (`it.length` → `Int`), so the call's result is `Int` and
    // assigning it where a `String` is expected is a type error the checker must report.
    let src = "class Box<T>(val v: T) { fun <R> map(f: (T) -> R): R = f(v) }\n\
fun main() { val b: Box<String> = Box(\"hi\"); val s: String = b.map { it.length }; println(s) }\n";
    let es = errors(src);
    assert!(
        es.iter().any(|m| m.contains("String") || m.contains("Int")),
        "expected a type-mismatch diagnostic for Int assigned to String, got: {es:?}"
    );
}
