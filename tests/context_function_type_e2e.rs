//! A context-receiver function TYPE (`context(C) () -> R`, `+ContextParameters`) has context receivers
//! as leading physical `FunctionN` parameters, while retaining which of those parameters are implicit
//! receivers in a lambda literal. It remains ABI-compatible with `(C) -> R`. Same-file, runs on the JVM.
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

#[test]
fn trailing_lambda_uses_context_receiver_implicitly() {
    const SRC: &str = "// LANGUAGE: +ContextParameters\n\
class Session(val token: String)\n\
inline fun <R> inspect(session: Session, crossinline action: context(Session) () -> R): R = action(session)\n\
fun box(): String {\n\
    val sessions = listOf(Session(\"OK\"))\n\
    return sessions.map { session ->\n\
        inspect(session) { token }\n\
    }.single()\n\
}\n";
    let diagnostics = common::checker_diags_with_stdlib(SRC).expect("stdlib");
    assert!(
        diagnostics.is_empty(),
        "unexpected diagnostics: {diagnostics:?}"
    );
    common::expect_box_ok_with_stdlib(SRC, "ContextLambda");
}

#[test]
fn multiple_context_receivers_are_implicit() {
    const SRC: &str = "// LANGUAGE: +ContextParameters\n\
class Left(val first: String)\n\
class Right(val second: String)\n\
inline fun <R> inspect(left: Left, right: Right, action: context(Left, Right) () -> R): R = action(left, right)\n\
fun box(): String = inspect(Left(\"O\"), Right(\"K\")) { first + second }\n";
    common::expect_box_ok_with_stdlib(SRC, "MultipleContextLambda");
}

#[test]
fn context_and_extension_receivers_are_implicit() {
    const SRC: &str = "// LANGUAGE: +ContextParameters\n\
class Prefix(val first: String)\n\
class Target(val second: String)\n\
inline fun <R> inspect(prefix: Prefix, target: Target, action: context(Prefix) Target.() -> R): R = action(prefix, target)\n\
fun box(): String = inspect(Prefix(\"O\"), Target(\"K\")) { first + second }\n";
    common::expect_box_ok_with_stdlib(SRC, "ContextExtensionLambda");
}

#[test]
fn non_inline_context_lambda_materializes_implicit_receiver() {
    const SRC: &str = "// LANGUAGE: +ContextParameters\n\
class Session(val token: String)\n\
fun <R> inspect(session: Session, action: context(Session) () -> R): R = action(session)\n\
fun box(): String = inspect(Session(\"OK\")) { token }\n";
    common::expect_box_ok_with_stdlib(SRC, "MaterializedContextLambda");
}

#[test]
fn non_inline_multiple_context_and_extension_receivers_materialize() {
    const SRC: &str = "// LANGUAGE: +ContextParameters\n\
class Left(val first: String)\n\
class Right(val second: String)\n\
class Target(val third: String)\n\
fun inspect(\n\
    left: Left,\n\
    right: Right,\n\
    target: Target,\n\
    action: context(Left, Right) Target.() -> String,\n\
): String = action(left, right, target)\n\
fun box(): String = inspect(Left(\"O\"), Right(\"K\"), Target(\"!\")) { first + second + third }\n";
    let output = run(SRC).expect("materialized context and extension lambda");
    assert_eq!(output, "OK!");
}

#[test]
fn suspend_context_lambda_materializes_implicit_receiver() {
    const SRC: &str = "// LANGUAGE: +ContextParameters\n\
class Session(val token: String)\n\
fun retain(seed: String, action: suspend context(Session) () -> String): String = seed\n\
fun box(): String = retain(\"OK\") { token }\n";
    common::expect_box_ok_with_stdlib(SRC, "SuspendMaterializedContextLambda");
}

#[test]
fn invoked_suspend_context_and_extension_of_same_type_use_distinct_storage() {
    const SRC: &str = "// LANGUAGE: +ContextParameters\n\
import kotlin.coroutines.Continuation\n\
import kotlin.coroutines.EmptyCoroutineContext\n\
import kotlin.coroutines.startCoroutine\n\
class Session(val token: String)\n\
fun <T> runNow(block: suspend () -> T): T {\n\
    var result: Result<T>? = null\n\
    block.startCoroutine(Continuation(EmptyCoroutineContext) { result = it })\n\
    return result!!.getOrThrow()\n\
}\n\
suspend fun execute(\n\
    context: Session,\n\
    target: Session,\n\
    action: suspend context(Session) Session.() -> String,\n\
): String = action(context, target)\n\
fun box(): String = runNow {\n\
    execute(Session(\"wrong\"), Session(\"OK\")) { token }\n\
}\n";
    common::expect_box_ok_with_stdlib(SRC, "InvokedSuspendContextExtensionLambda");
}

#[test]
fn overloaded_cross_file_context_lambda_is_target_typed() {
    const HELPERS: &str = "// LANGUAGE: +ContextParameters\n\
class Element\n\
class Module\n\
class Session(val token: String)\n\
inline fun <R> inspect(element: Element, crossinline action: context(Session) () -> R): R = action(Session(\"wrong\"))\n\
inline fun <R> inspect(module: Module, crossinline action: context(Session) () -> R): R = action(Session(\"OK\"))\n";
    const CALLER: &str = "// LANGUAGE: +ContextParameters\n\
fun box(): String = listOf(Module()).map { module ->\n\
    inspect(module) { token }\n\
}.single()\n";
    common::expect_front_end_ok_files_with_stdlib(&[HELPERS, CALLER], "CrossFileContextLambda");
}

#[test]
fn source_context_shape_is_not_mixed_with_classpath_encoding() {
    const HELPERS: &str = "// LANGUAGE: +ContextParameters\n\
package sample\n\
class Element\n\
class Module\n\
class Session(val token: String)\n\
inline fun <R> inspect(element: Element, crossinline action: context(Session) () -> R): R = action(Session(\"wrong\"))\n\
inline fun <R> inspect(module: Module, crossinline action: context(Session) () -> R): R = action(Session(\"OK\"))\n";
    const CALLER: &str = "// LANGUAGE: +ContextParameters\n\
package sample\n\
fun consume(module: Module): String = inspect(module) { token }\n";
    let Some(dependency) = common::compile_lib("context_shape_duplicate", HELPERS) else {
        return;
    };
    let Some(stdlib) = common::stdlib_jar() else {
        return;
    };
    let classpath = [dependency, stdlib];
    let diagnostics = common::front_end_diagnostics_files(
        &[HELPERS, CALLER],
        &classpath,
        common::jdk_modules().as_deref(),
    );
    assert!(
        diagnostics.is_empty(),
        "unexpected diagnostics: {diagnostics:?}"
    );
}

#[test]
fn sibling_generic_member_calls_keep_erased_return_descriptors() {
    const DECLARATION: &str = "package sample\n\
class Transformer {\n\
    inline fun <R> produce(action: () -> R): R = action()\n\
}\n\
class DefaultTransformer {\n\
    fun <R> produce(prefix: String = \"\", action: () -> R): R = action()\n\
}\n\
class IndexedTransformer {\n\
    operator fun <R> get(action: () -> R): R = action()\n\
}\n";
    const USE: &str = "package sample\n\
fun box(): String {\n\
    val direct = Transformer().produce { \"O\" }\n\
    val defaulted = DefaultTransformer().produce { \"K\" }\n\
    val indexed = IndexedTransformer()[{ \"!\" }]\n\
    return if (direct + defaulted + indexed == \"OK!\") \"OK\" else \"no\"\n\
}\n";
    let output = common::compile_and_run_files_with_stdlib(&[
        ("Declaration.kt", DECLARATION),
        ("Use.kt", USE),
    ])
    .expect("multi-file generic member");
    assert_eq!(output, "OK");
}

#[test]
fn context_call_keeps_classpath_map_not_null_element() {
    const LIB: &str = "package sample\n\
interface Entry\n\
class Wrapped(val module: Module) : Entry\n\
class Module\n\
class Document(val entries: Array<Entry>)\n";
    const MAIN: &str = "// LANGUAGE: +ContextParameters\n\
import sample.Document\n\
import sample.Entry\n\
import sample.Module\n\
import sample.Wrapped\n\
class Session(val token: String)\n\
inline fun <R> inspect(element: Entry, crossinline action: context(Session) () -> R): R = action(Session(\"wrong\"))\n\
inline fun <R> inspect(module: Module, crossinline action: context(Session) () -> R): R = action(Session(\"OK\"))\n\
fun consume(document: Document): String {\n\
    val modules = document.entries.mapNotNullTo(mutableSetOf()) { entry ->\n\
        (entry as? Wrapped)?.module\n\
    }\n\
    return modules.map { module -> inspect(module) { token } }.single()\n\
}\n";
    let Some(diagnostics) = common::checker_diags_against("context_collection_chain", LIB, MAIN)
    else {
        return;
    };
    assert!(
        diagnostics.is_empty(),
        "unexpected diagnostics: {diagnostics:?}"
    );

    const EXPLICIT: &str = "// LANGUAGE: +ContextParameters\n\
import sample.Document\n\
import sample.Entry\n\
import sample.Module\n\
import sample.Wrapped\n\
class Session(val token: String)\n\
inline fun <R> inspect(element: Entry, crossinline action: context(Session) () -> R): R = action(Session(\"wrong\"))\n\
inline fun <R> inspect(module: Module, crossinline action: context(Session) () -> R): R = action(Session(\"OK\"))\n\
fun consume(document: Document): String {\n\
    val modules = document.entries.mapNotNullTo(mutableSetOf<Any>()) { entry ->\n\
        (entry as? Wrapped)?.module\n\
    }\n\
    return modules.map { module -> inspect(module) { token } }.single()\n\
}\n";
    let Some(explicit_diagnostics) =
        common::checker_diags_against("context_collection_explicit_any", LIB, EXPLICIT)
    else {
        return;
    };
    assert!(
        explicit_diagnostics
            .iter()
            .any(|diagnostic| diagnostic.contains("candidates")),
        "explicit Any must not be narrowed: {explicit_diagnostics:?}"
    );
}
