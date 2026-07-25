//! A context-receiver function TYPE (`context(C) () -> R`, `+ContextParameters`) is modeled as a plain
//! function type with the context receivers as LEADING parameters — identical to `(C) -> R`. So a plain
//! function value converts to a context-function-typed parameter, and invoking it passes the context as
//! the first argument. Same-file, runs on the JVM.
use super::common;
fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn plain_function_converts_to_context_function_type() {
    const SRC: &str = "// LANGUAGE: +ContextParameters\n\
        fun withContext(f: context(String) () -> String) = f(\"OK\")\n\
        fun callWithContext(f: (String) -> String) = withContext(f)\n\
        fun box(): String = callWithContext { s -> s }\n";
    assert_eq!(run(SRC).expect("context-fn-type conversion"), "OK");
}

#[test]
fn context_and_value_params_flatten() {
    // `context(A) (B) -> R` ≡ `(A, B) -> R`: the context receiver precedes the value parameters.
    const SRC: &str = "// LANGUAGE: +ContextParameters\n\
        fun apply2(f: context(Int) (Int) -> Int) = f(10, 5)\n\
        fun box(): String {\n\
        \x20 val g: (Int, Int) -> Int = { a, b -> a - b }\n\
        \x20 return if (apply2(g) == 5) \"OK\" else \"no\"\n\
        }\n";
    assert_eq!(run(SRC).expect("context + value params"), "OK");
}

/// A context receiver on an EXTENSION function type (`context(S) Int.(Long) -> R`) folds BOTH the
/// context receiver and the extension receiver in as leading parameters — `(S, Int, Long) -> R`. A
/// plain function reference of that shape converts to it. Previously the parser rejected this type.
#[test]
fn context_receiver_on_extension_function_type() {
    const SRC: &str = "// LANGUAGE: +ContextParameters\n\
fun helper(s: String, i: Int, l: Long): Int = i + l.toInt() + s.length\n\
fun use(f: context(String) Int.(Long) -> Int): Int = f(\"ctx\", 5, 3L)\n\
fun box(): String = if (use(::helper) == 11) \"OK\" else \"no\"\n";
    assert_eq!(
        run(SRC).expect("context receiver on extension function type"),
        "OK"
    );
}

#[test]
fn applicable_callable_wins_between_local_function_and_property() {
    const SRC: &str = "// LANGUAGE: +ContextParameters\n\
fun a(a: String): String = \"top-level fun\"\n\
fun invokeA(): String {\n\
    val a: context(String) () -> String = { \"property a\" }\n\
    return a(\"1\")\n\
}\n\
val b: context(String) () -> String = { \"property b\" }\n\
fun invokeB(): String {\n\
    fun b(a: String): String = \"local fun\"\n\
    return b(\"1\")\n\
}\n\
val c: context(String) () -> String = { \"property c\" }\n\
fun invokeC(): String {\n\
    context(a: String)\n\
    fun c(): String = \"local context fun\"\n\
    return c(\"1\")\n\
}\n\
val d: () -> String = { \"property d\" }\n\
fun invokeD(): String {\n\
    context(s: String)\n\
    fun d(): String = \"local context fun\"\n\
    return d()\n\
}\n\
fun box(): String = if (invokeA() == \"property a\" && invokeB() == \"local fun\" && invokeC() == \"property c\" && invokeD() == \"property d\") \"OK\" else \"no\"\n";
    assert_eq!(
        run(SRC).expect("local function/property applicability"),
        "OK"
    );
}

#[test]
fn local_overload_requires_available_context() {
    const SRC: &str = "// LANGUAGE: +ContextParameters\n\
class A\n\
class B\n\
fun box(): String {\n\
    val b = B()\n\
    context(a: A)\n\
    fun pick(value: String): String = \"wrong\"\n\
    context(b: B)\n\
    fun pick(value: Any): String = \"OK\"\n\
    return pick(\"value\")\n\
}\n";
    assert_eq!(
        run(SRC).expect("local overload context applicability"),
        "OK"
    );
}
