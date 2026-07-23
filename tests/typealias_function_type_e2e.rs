//! A source `typealias` whose target is a FUNCTION type (`typealias Listener = (String) -> Unit`,
//! `typealias Handler = suspend (String) -> Unit`) used in type positions: parameters, properties,
//! and lambda binding. Corpus: coroutines/inlineSuspendTypealias.kt,
//! suspendConversion/suspendConversionOfAliasedType.kt.
use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Ta")
}

#[test]
fn fun_type_alias_as_param_and_local() {
    const SRC: &str = "typealias Listener = (String) -> Unit\n\
fun call(l: Listener) { l(\"OK\") }\n\
fun box(): String {\n\
    var got = \"FAIL\"\n\
    val f: Listener = { got = it }\n\
    call(f)\n\
    return got\n\
}\n";
    let out = run(SRC).expect("function-type alias should compile + run");
    assert_eq!(out, "OK");
}

#[test]
fn fun_type_alias_with_return_value() {
    const SRC: &str = "typealias Mapper = (Int) -> String\n\
fun apply(m: Mapper, x: Int): String = m(x)\n\
fun box(): String {\n\
    val m: Mapper = { \"v\" + it }\n\
    if (apply(m, 7) != \"v7\") return \"fail apply\"\n\
    return \"OK\"\n\
}\n";
    let out = run(SRC).expect("value-returning function-type alias should compile + run");
    assert_eq!(out, "OK");
}

#[test]
fn context_receiver_fun_type_alias() {
    // `typealias H = context(A) (B) -> R` — a context-receiver function-type target must also be
    // detected as a function type (it is modeled as `(A, B) -> R` with the context leading).
    const SRC: &str = "// LANGUAGE: +ContextParameters\n\
typealias Op = context(Int) (Int) -> Int\n\
fun apply2(f: Op) = f(10, 5)\n\
fun box(): String {\n\
    val g: (Int, Int) -> Int = { a, b -> a - b }\n\
    return if (apply2(g) == 5) \"OK\" else \"no\"\n\
}\n";
    let out = run(SRC).expect("context-receiver function-type alias should compile + run");
    assert_eq!(out, "OK");
}

#[test]
fn suspend_fun_type_alias_as_param() {
    // Corpus coroutines/inlineSuspendTypealias.kt shape: an alias of a SUSPEND function type as a
    // parameter, invoked from a coroutine started via startCoroutine. (The lambda RETURNS its value —
    // a var-capture write inside a nested suspend lambda is a separate unsupported suspend shape,
    // with or without the alias.)
    const SRC: &str = "import kotlin.coroutines.*\n\
typealias Handler = suspend (String) -> String\n\
suspend fun foo(h: Handler): String = h(\"O\")\n\
fun box(): String {\n\
    var res = \"FAIL\"\n\
    val block: suspend () -> String = { foo { it + \"K\" } }\n\
    block.startCoroutine(Continuation(EmptyCoroutineContext) { res = it.getOrThrow() })\n\
    return res\n\
}\n";
    let out = run(SRC).expect("suspend function-type alias should compile + run");
    assert_eq!(out, "OK");
}
