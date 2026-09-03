//! kotlinc mangles a suspend function's JVM name from its ORIGINAL signature, which carries a trailing
//! `Continuation` value parameter (a non-inline `_` element). So a suspend `create(id: Id)` mangles
//! differently from the non-suspend overload. krusty omitted the `Continuation` element, producing the
//! wrong hash (and one that collided with the non-suspend mangle). This drives a suspend value-class
//! method end-to-end (same-file call → the decl and the call must agree on the mangled name).

use super::common;

fn run(tag: &str, main: &str) -> Option<String> {
    let jdk = common::jdk_modules();
    let sl = common::stdlib_jar();
    let coro = common::coroutines_jar();
    let lo = common::compile_lib(
        tag,
        "package lib\ninterface Dep { suspend fun ping(): Int }\n\
         object D : Dep { override suspend fun ping(): Int = 1 }\n",
    )?;
    common::compile_and_run_box(
        main,
        "Main",
        &[lo, sl, coro, jdk.clone()],
        Some(jdk.as_path()),
    )
}

#[test]
fn suspend_value_class_param_method_runs() {
    // `create` takes a value class `Id` and suspends (calls `D.ping()`), so kotlinc mangles its name.
    // The box() call is same-file — index-resolved — so it must target the same mangled method.
    const MAIN: &str = "import lib.*\n\
        import kotlinx.coroutines.runBlocking\n\
        @JvmInline value class Id(val v: String)\n\
        class C(val d: Dep) { suspend fun create(id: Id): Int { d.ping(); return id.v.length } }\n\
        fun box(): String = runBlocking { if (C(D).create(Id(\"abc\")) == 3) \"OK\" else \"F\" }\n";
    assert_eq!(run("svcm", MAIN).expect("suspend value-class method"), "OK");
}

#[test]
fn suspend_value_class_result_is_unboxed_after_resume() {
    let jdk = common::jdk_modules();
    let sl = common::stdlib_jar();
    let coro = common::coroutines_jar();
    const MAIN: &str = "import kotlinx.coroutines.runBlocking\n\
        @JvmInline value class Id(val value: String)\n\
        suspend fun make(): Id = Id(\"OK\")\n\
        suspend fun consume(): String { val id = make(); return id.value }\n\
        fun box(): String = runBlocking { consume() }\n";
    let out =
        common::compile_and_run_box(MAIN, "Main", &[sl, coro, jdk.clone()], Some(jdk.as_path()));
    assert_eq!(out.as_deref(), Some("OK"));
}

#[test]
fn generic_suspend_override_bridge_delegates_to_mangled_value_class_result() {
    let jdk = common::jdk_modules();
    let sl = common::stdlib_jar();
    let coro = common::coroutines_jar();
    const MAIN: &str = "import kotlinx.coroutines.runBlocking\n\
        @JvmInline value class Id(val value: String)\n\
        interface Source<T> { suspend fun get(): T }\n\
        class Actual : Source<Id> { override suspend fun get(): Id = Id(\"OK\") }\n\
        suspend fun consume(source: Source<Id>): String = source.get().value\n\
        fun box(): String = runBlocking { consume(Actual()) }\n";
    let out =
        common::compile_and_run_box(MAIN, "Main", &[sl, coro, jdk.clone()], Some(jdk.as_path()));
    assert_eq!(out.as_deref(), Some("OK"));
}

#[test]
fn nullable_value_class_suspend_bridge_resumes_through_not_null_assertion() {
    let jdk = common::jdk_modules();
    let sl = common::stdlib_jar();
    let coro = common::coroutines_jar();
    const MAIN: &str = "import kotlinx.coroutines.runBlocking\n\
        import kotlinx.coroutines.yield\n\
        @JvmInline value class Id(val value: String)\n\
        interface Source { suspend fun get(): Id? }\n\
        class Actual : Source { override suspend fun get(): Id { yield(); return Id(\"OK\") } }\n\
        suspend fun consume(source: Source): String = source.get()!!.value\n\
        fun box(): String = runBlocking { consume(Actual()) }\n";
    let out =
        common::compile_and_run_box(MAIN, "Main", &[sl, coro, jdk.clone()], Some(jdk.as_path()));
    assert_eq!(out.as_deref(), Some("OK"));
}

#[test]
fn nullable_null_capable_value_class_suspend_bridge_preserves_box() {
    let jdk = common::jdk_modules();
    let sl = common::stdlib_jar();
    let coro = common::coroutines_jar();
    const MAIN: &str = "import kotlinx.coroutines.runBlocking\n\
        import kotlinx.coroutines.yield\n\
        @JvmInline value class Id(val value: Int?)\n\
        interface Source { suspend fun get(): Id? }\n\
        class Actual : Source { override suspend fun get(): Id { yield(); return Id(42) } }\n\
        suspend fun consume(source: Source): String = if (source.get()!!.value == 42) \"OK\" else \"FAIL\"\n\
        fun box(): String = runBlocking { consume(Actual()) }\n";
    let out =
        common::compile_and_run_box(MAIN, "Main", &[sl, coro, jdk.clone()], Some(jdk.as_path()));
    assert_eq!(out.as_deref(), Some("OK"));
}

#[test]
fn suspend_override_preserves_value_class_identity_on_same_carrier_descriptor() {
    let jdk = common::jdk_modules();
    let sl = common::stdlib_jar();
    let coro = common::coroutines_jar();
    const MAIN: &str = "import kotlinx.coroutines.runBlocking\n\
        import kotlinx.coroutines.yield\n\
        interface Marker\n\
        class Payload(val value: String) : Marker\n\
        @JvmInline value class Id(val value: Marker) : Marker\n\
        interface Source { suspend fun get(): Marker? }\n\
        class Actual : Source { override suspend fun get(): Id { yield(); return Id(Payload(\"OK\")) } }\n\
        suspend fun consume(source: Source): String = ((source.get() as Id).value as Payload).value\n\
        fun box(): String = runBlocking { consume(Actual()) }\n";
    let out =
        common::compile_and_run_box(MAIN, "Main", &[sl, coro, jdk.clone()], Some(jdk.as_path()));
    assert_eq!(out.as_deref(), Some("OK"));
}

#[test]
fn generic_value_class_suspend_override_uses_safe_coroutine_protocol() {
    let jdk = common::jdk_modules();
    let sl = common::stdlib_jar();
    let coro = common::coroutines_jar();
    const MAIN: &str = "import kotlinx.coroutines.runBlocking\n\
        import kotlin.coroutines.suspendCoroutine\n\
        @JvmInline value class Id(val value: String)\n\
        interface Source<T> { suspend fun get(): T }\n\
        class Actual : Source<Id> {\n\
            override suspend fun get(): Id = suspendCoroutine { it.resumeWith(Result.success(Id(\"OK\"))) }\n\
        }\n\
        fun box(): String = runBlocking { val source: Source<*> = Actual(); (source.get() as Id).value }\n";
    let out =
        common::compile_and_run_box(MAIN, "Main", &[sl, coro, jdk.clone()], Some(jdk.as_path()));
    assert_eq!(out.as_deref(), Some("OK"));
}

#[test]
fn suspended_result_value_class_survives_value_try_and_catch() {
    let jdk = common::jdk_modules();
    let sl = common::stdlib_jar();
    let coro = common::coroutines_jar();
    const MAIN: &str = "import kotlin.coroutines.*\n\
        var pending: Continuation<String>? = null\n\
        var observed = \"unset\"\n\
        fun builder(block: suspend () -> Unit) {\n\
            block.startCoroutine(Continuation(EmptyCoroutineContext) { it.getOrThrow() })\n\
        }\n\
        suspend fun pause(): String = suspendCoroutine { pending = it }\n\
        @Suppress(\"RESULT_CLASS_IN_RETURN_TYPE\")\n\
        suspend fun outcome(): Result<String> = try {\n\
            Result.success(pause())\n\
        } catch (failure: Exception) {\n\
            Result.failure(failure)\n\
        }\n\
        fun launch() { builder { val result = outcome(); observed = result.exceptionOrNull()?.message ?: result.getOrThrow() } }\n\
        fun box(): String {\n\
            launch(); pending!!.resume(\"first\")\n\
            if (observed != \"first\") return \"success: $observed\"\n\
            launch(); pending!!.resumeWithException(Exception(\"OK\"))\n\
            return observed\n\
        }\n";
    let out =
        common::compile_and_run_box(MAIN, "Main", &[sl, coro, jdk.clone()], Some(jdk.as_path()));
    assert_eq!(out.as_deref(), Some("OK"));
}

#[test]
fn resumed_value_class_survives_non_local_return_from_inline_run() {
    let jdk = common::jdk_modules();
    let sl = common::stdlib_jar();
    let coro = common::coroutines_jar();
    const MAIN: &str = "import kotlin.coroutines.*\n\
        @JvmInline value class Id(val value: String)\n\
        var pending: Continuation<Id>? = null\n\
        var observed = \"FAIL\"\n\
        fun builder(block: suspend () -> Unit) {\n\
            block.startCoroutine(Continuation(EmptyCoroutineContext) { it.getOrThrow() })\n\
        }\n\
        suspend fun pause(): Id = suspendCoroutine { pending = it }\n\
        class Source {\n\
            suspend fun <T> identity(value: T): T = value\n\
            suspend fun get(): Id { run { return identity(pause()) } }\n\
        }\n\
        fun box(): String {\n\
            builder { observed = Source().get().value }\n\
            if (observed != \"FAIL\") return \"completed before resume: $observed\"\n\
            pending!!.resume(Id(\"OK\"))\n\
            return observed\n\
        }\n";
    let out =
        common::compile_and_run_box(MAIN, "Main", &[sl, coro, jdk.clone()], Some(jdk.as_path()));
    assert_eq!(out.as_deref(), Some("OK"));
}

#[test]
fn static_value_class_suspend_member_uses_carrier_as_slot_zero() {
    let jdk = common::jdk_modules();
    let sl = common::stdlib_jar();
    let coro = common::coroutines_jar();
    const MAIN: &str = "import kotlin.coroutines.*\n\
        @JvmInline value class ResultId(val value: Any?)\n\
        var pending: Continuation<ResultId>? = null\n\
        var observed = \"FAIL\"\n\
        fun builder(block: suspend () -> Unit) {\n\
            block.startCoroutine(Continuation(EmptyCoroutineContext) { it.getOrThrow() })\n\
        }\n\
        @JvmInline value class SourceId(val value: String) {\n\
            suspend operator fun invoke(): ResultId = suspendCoroutine { pending = it }\n\
        }\n\
        fun box(): String {\n\
            builder { observed = SourceId(\"source\")().value as String }\n\
            if (observed != \"FAIL\") return \"completed before resume: $observed\"\n\
            pending!!.resume(ResultId(\"OK\"))\n\
            return observed\n\
        }\n";
    let out =
        common::compile_and_run_box(MAIN, "Main", &[sl, coro, jdk.clone()], Some(jdk.as_path()));
    assert_eq!(out.as_deref(), Some("OK"));
}
