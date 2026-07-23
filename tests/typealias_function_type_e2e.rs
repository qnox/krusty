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

#[test]
fn generic_fun_type_alias_substitutes_use_site_args() {
    // `typealias Mapper<T, R> = (T) -> R` — the use site's type arguments substitute into the
    // target's type-parameter references during the parse-seam expansion.
    const SRC: &str = "typealias Mapper<T, R> = (T) -> R\n\
fun apply(m: Mapper<Int, String>, x: Int): String = m(x)\n\
fun box(): String {\n\
    val m: Mapper<Int, String> = { \"v\" + it }\n\
    return if (apply(m, 7) == \"v7\") \"OK\" else \"no\"\n\
}\n";
    let out = run(SRC).expect("generic function-type alias should compile + run");
    assert_eq!(out, "OK");
}

#[test]
fn generic_suspend_fun_type_alias() {
    // A GENERIC alias of a SUSPEND function type (deserializedSuspendFunctionProperty.kt-class
    // shape, minus the receiver form — invoking a receiver-fn-typed PARAM is a separate,
    // pre-existing gap with or without the alias).
    const SRC: &str = "import kotlin.coroutines.*\n\
typealias Op<T, R> = suspend (T) -> R\n\
fun runIt(block: suspend () -> String): String {\n\
    var res = \"\"\n\
    block.startCoroutine(Continuation(EmptyCoroutineContext) { res = it.getOrThrow() })\n\
    return res\n\
}\n\
suspend fun call(f: Op<String, String>): String = f(\"O\")\n\
fun box(): String {\n\
    val f: Op<String, String> = { it + \"K\" }\n\
    return runIt { call(f) }\n\
}\n";
    let out = run(SRC).expect("generic suspend function-type alias should compile + run");
    assert_eq!(out, "OK");
}

#[test]
fn class_target_alias_with_fn_type_argument_is_preserved() {
    // `typealias Handlers = Map<String, (Int) -> Int>` — the `->` inside the CLASS target's type
    // argument must not reroute the alias out of the plain class-name map (a regression the
    // arrow-scan detection would otherwise introduce: the alias silently dropped).
    const SRC: &str = "class Registry\n\
typealias Reg = Registry\n\
typealias Handlers = Map<String, (Int) -> Int>\n\
fun useReg(r: Reg): String = \"OK\"\n\
fun size(h: Handlers): Int = h.size\n\
fun box(): String {\n\
    if (useReg(Registry()) != \"OK\") return \"fail reg\"\n\
    val h: Handlers = mapOf(\"d\" to { n: Int -> n * 2 })\n\
    if (size(h) != 1) return \"fail handlers\"\n\
    return \"OK\"\n\
}\n";
    let out = run(SRC).expect("class-target alias with fn-type argument should compile + run");
    assert_eq!(out, "OK");
}
