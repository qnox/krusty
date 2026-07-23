//! Suspend conversion: a NON-suspend function VALUE flowing into a `suspend` function-type parameter
//! is wrapped in a synthesized adapter (kotlinc: a `FunctionReferenceImpl` subclass implementing
//! `Function{n+1}` + the `SuspendFunction` marker, whose `invoke` delegates to the wrapped
//! `Function{n}.invoke` and ignores the continuation — a plain function never suspends).
//! Corpus: suspendConversion/*, e.g. suspendConversionOfAliasedType.kt.
use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Sc")
}

#[test]
fn plain_fn_value_converts_to_suspend_param() {
    // The suspend-fn-value ARG to `call` also exercises the suspend→suspend identity pass-through.
    // (Invoking the converted value directly inside the suspend LAMBDA is a separate unlowered
    // shape — the named suspend fn `call` carries the invoke instead.)
    const SRC: &str = "import kotlin.coroutines.*\n\
fun runInt(block: suspend () -> Int): Int {\n\
    var res = 0\n\
    block.startCoroutine(Continuation(EmptyCoroutineContext) { res = it.getOrThrow() })\n\
    return res\n\
}\n\
suspend fun call(f: suspend (Int) -> Int): Int = f(20)\n\
fun runs(f: suspend (Int) -> Int): Int = runInt { call(f) }\n\
fun box(): String {\n\
    val g: (Int) -> Int = { it + 22 }\n\
    return if (runs(g) == 42) \"OK\" else \"no\"\n\
}\n";
    let out = run(SRC).expect("suspend conversion of a plain fn value should compile + run");
    assert_eq!(out, "OK");
}

#[test]
fn unit_returning_fn_value_converts_to_suspend_param() {
    // The Unit case: the erased Function1.invoke of a Unit lambda returns Unit.INSTANCE, which is
    // exactly the completion value the resumed coroutine expects — pass-through, no special casing.
    const SRC: &str = "import kotlin.coroutines.*\n\
var got = \"FAIL\"\n\
fun runIt(block: suspend () -> Int): Int {\n\
    var res = 0\n\
    block.startCoroutine(Continuation(EmptyCoroutineContext) { res = it.getOrThrow() })\n\
    return res\n\
}\n\
suspend fun call(f: suspend (String) -> Unit) = f(\"OK\")\n\
fun accept(f: suspend (String) -> Unit): Int = runIt { call(f); 1 }\n\
fun box(): String {\n\
    val g: (String) -> Unit = { got = it }\n\
    accept(g)\n\
    return got\n\
}\n";
    let out = run(SRC).expect("suspend conversion of a Unit fn value should compile + run");
    assert_eq!(out, "OK");
}

#[test]
fn conversion_where_param_is_typealias_of_plain_fn_type() {
    // The corpus suspendConversionOfAliasedType.kt shape: the VALUE's type is a typealias of a plain
    // function type; the parameter is a suspend function type.
    const SRC: &str = "typealias Listener = (String) -> Unit\n\
fun foo(f: suspend (String) -> Unit): String = \"OK\"\n\
fun box(): String {\n\
    val f: Listener = {}\n\
    return foo(f)\n\
}\n";
    let out = run(SRC).expect("suspend conversion of an aliased fn value should compile + run");
    assert_eq!(out, "OK");
}
