//! How a value-class result crosses a suspension, compared against kotlinc.
//!
//! A suspend function whose declared result is a value class returns its carrier where it does not
//! suspend, and its continuation hands the value to the completion as the box. A caller that resumes
//! with the box unboxes it, so it cannot forward its own continuation to such a call. A function that
//! overrides a declaration returning a type parameter returns the box instead, and a call to a callee
//! returning a type parameter receives the box on either path, so such a call stays a tail call.

use super::common;

fn expect_method_matches(src: &str, class: &str, method: &str) {
    expect_method_matches_over(&[], src, class, method);
}

fn expect_method_matches_over(lib: &[(&str, &str)], src: &str, class: &str, method: &str) {
    match common::method_code_diff_against_kotlinc(
        "SuspendValueClassResults",
        lib,
        src,
        class,
        method,
    ) {
        None => panic!("reference kotlinc is provisioned"),
        Some(Ok(())) => {}
        Some(Err(difference)) => panic!("{difference}"),
    }
}

const RESULTS: &str = "@JvmInline value class Name(val s: String)\n\
interface Gate { suspend fun open(): Int }\n\
suspend fun get(g: Gate): Name { g.open(); return Name(\"x\") }\n\
suspend fun maybe(g: Gate, n: Int): Name? { g.open(); return if (n > 0) Name(\"y\") else null }\n\
suspend fun forward(g: Gate): String = get(g).s\n";

/// The continuation boxes the carrier after the `COROUTINE_SUSPENDED` check, and keeps a `null`
/// of a nullable carrier as it is.
#[test]
fn a_continuation_hands_a_value_class_result_to_its_completion_boxed() {
    for class in [
        "SuspendValueClassResultsKt$get$1",
        "SuspendValueClassResultsKt$maybe$1",
    ] {
        expect_method_matches(
            RESULTS,
            class,
            "public final java.lang.Object invokeSuspend(java.lang.Object);",
        );
    }
}

/// `forward` unboxes the box it resumes with, so it keeps a continuation of its own instead of
/// handing `get` the one it was given.
#[test]
fn a_call_resumed_with_a_boxed_value_class_is_not_a_tail_call() {
    expect_method_matches(
        RESULTS,
        "SuspendValueClassResultsKt$forward$1",
        "public final java.lang.Object invokeSuspend(java.lang.Object);",
    );
}

const OVERRIDES: &str = "@JvmInline value class Name(val s: String)\n\
interface Gate { suspend fun open(): Int }\n\
suspend fun <T> pick(g: Gate, v: T): T { g.open(); return v }\n\
interface Base<T : Name?> { suspend fun generic(): T }\n\
class Derived(val g: Gate) : Base<Name> { override suspend fun generic(): Name = pick(g, Name(\"z\")) }\n\
suspend fun viaBase(b: Base<*>): String = b.generic()!!.s\n";

/// `Derived.generic` overrides a declaration returning a type parameter, so it returns the box that
/// the generic `pick` completes with and forwards its continuation to it.
#[test]
fn an_override_of_a_type_parameter_result_returns_the_box_of_a_generic_call() {
    expect_method_matches(
        OVERRIDES,
        "Derived",
        "public java.lang.Object generic-t1DQ2nc(kotlin.coroutines.Continuation<? super Name>);",
    );
}

/// A call through the type parameter's declaration unboxes the box it receives on either path.
#[test]
fn a_call_to_a_type_parameter_result_unboxes_it() {
    expect_method_matches(
        OVERRIDES,
        "SuspendValueClassResultsKt$viaBase$1",
        "public final java.lang.Object invokeSuspend(java.lang.Object);",
    );
}

const DEPENDENCY_BASE: &str = "package dep\n\
interface Base<T> { suspend fun value(): T }\n";

const DEPENDENCY_OVERRIDE: &str = "import dep.*\n\
@JvmInline value class Name(val s: String)\n\
interface Gate { suspend fun open(): Int }\n\
suspend fun <T> pick(g: Gate, v: T): T { g.open(); return v }\n\
class Derived(val g: Gate) : Base<Name> { override suspend fun value(): Name = pick(g, Name(\"z\")) }\n";

/// A dependency's declaration counts like a module's: overriding its type-parameter result makes
/// `Derived.value` return the box, forwarding its continuation to the generic `pick`.
#[test]
fn an_override_of_a_dependency_type_parameter_result_returns_the_box() {
    expect_method_matches_over(
        &[("Lib.kt", DEPENDENCY_BASE)],
        DEPENDENCY_OVERRIDE,
        "Derived",
        "public java.lang.Object value-t1DQ2nc(kotlin.coroutines.Continuation<? super Name>);",
    );
}

/// A caller of the dependency's `Base.value` receives the box whether `Derived` completes at once
/// or suspends and is resumed later.
#[test]
fn a_dependency_override_hands_its_callers_the_box_on_either_path() {
    let main = format!(
        "{DEPENDENCY_OVERRIDE}\
         import kotlin.coroutines.*\n\
         import kotlin.coroutines.intrinsics.*\n\
         var parked: Continuation<Int>? = null\n\
         class Immediate : Gate {{ override suspend fun open(): Int = 1 }}\n\
         class Parking : Gate {{\n\
         \x20   override suspend fun open(): Int = suspendCoroutineUninterceptedOrReturn {{ parked = it; COROUTINE_SUSPENDED }}\n\
         }}\n\
         suspend fun read(base: Base<Name>): String = base.value().s\n\
         fun box(): String {{\n\
         \x20   var result = \"\"\n\
         \x20   val body: suspend () -> Unit = {{\n\
         \x20       result += read(Derived(Immediate()))\n\
         \x20       result += read(Derived(Parking()))\n\
         \x20   }}\n\
         \x20   body.startCoroutine(Continuation(EmptyCoroutineContext) {{ it.getOrThrow() }})\n\
         \x20   parked!!.resume(2)\n\
         \x20   return if (result == \"zz\") \"OK\" else \"result $result\"\n\
         }}\n"
    );
    assert_eq!(
        common::expect_box_run_against_ref("dependency-override-results", DEPENDENCY_BASE, &main)
            .as_deref(),
        Some("OK")
    );
}

const DEPENDENCY_IMPLEMENTATION: &str = "package dep\n\
class ResultPayload(val text: String)\n\
@JvmInline value class ResultTicket(val payload: ResultPayload)\n\
interface Gate { suspend fun open(): Int }\n\
interface Base<T> { suspend fun value(): T }\n\
class Impl(val g: Gate) : Base<ResultTicket> { override suspend fun value(): ResultTicket { g.open(); return ResultTicket(ResultPayload(\"z\")) } }\n";

/// A dependency's `Impl.value` overrides `Base<T>.value(): T`, so it hands over the box on either
/// path however its caller names it; a caller returning `Any` forwards its continuation to it.
#[test]
fn a_dependency_override_of_a_type_parameter_result_hands_over_the_box() {
    expect_method_matches_over(
        &[("Lib.kt", DEPENDENCY_IMPLEMENTATION)],
        "import dep.*\n\
         suspend fun viaImpl(i: Impl): Any = i.value()\n",
        "SuspendValueClassResultsKt",
        "public static final java.lang.Object viaImpl(",
    );
}

/// A caller of the dependency's `Impl.value` unboxes the box, and a caller returning `Any` hands it
/// on, whether the call completes at once or suspends and is resumed later.
#[test]
fn a_dependency_override_is_unboxed_on_either_path() {
    let main = "import dep.*\n\
         import kotlin.coroutines.*\n\
         import kotlin.coroutines.intrinsics.*\n\
         var parked: Continuation<Int>? = null\n\
         class Immediate : Gate { override suspend fun open(): Int = 1 }\n\
         class Parking : Gate {\n\
         \x20   override suspend fun open(): Int = suspendCoroutineUninterceptedOrReturn { parked = it; COROUTINE_SUSPENDED }\n\
         }\n\
         suspend fun read(i: Impl): String = i.value().payload.text\n\
         suspend fun viaImpl(i: Impl): Any = i.value()\n\
         fun box(): String {\n\
         \x20   var result = \"\"\n\
         \x20   val body: suspend () -> Unit = {\n\
         \x20       result += read(Impl(Immediate()))\n\
         \x20       result += read(Impl(Parking()))\n\
         \x20       result += (viaImpl(Impl(Immediate())) as ResultTicket).payload.text\n\
         \x20       result += (viaImpl(Impl(Parking())) as ResultTicket).payload.text\n\
         \x20   }\n\
         \x20   body.startCoroutine(Continuation(EmptyCoroutineContext) { it.getOrThrow() })\n\
         \x20   parked!!.resume(2)\n\
         \x20   parked!!.resume(3)\n\
         \x20   return if (result == \"zzzz\") \"OK\" else \"result $result\"\n\
         }\n";
    assert_eq!(
        common::expect_box_run_against_ref(
            "dependency-implementation-results",
            DEPENDENCY_IMPLEMENTATION,
            main
        )
        .as_deref(),
        Some("OK")
    );
}

const PRIVATE_MEMBER: &str = "class Holder {\n\
    private suspend fun h(x: Int): String = \"OK\"\n\
    fun k(): suspend () -> String = { h(1) }\n\
}\n\
fun box(): String { Holder().k(); return \"OK\" }\n";

/// A private suspend member called from a suspend lambda class gets one `access$` bridge, which
/// also serves as its continuation's re-entry. A second one would stop `Holder` from loading.
#[test]
fn a_private_suspend_member_called_from_a_lambda_class_has_one_access_bridge() {
    expect_method_matches(
        PRIVATE_MEMBER,
        "Holder",
        "public static final java.lang.Object access$h(Holder, int, kotlin.coroutines.Continuation);",
    );
    let jdk = common::jdk_modules();
    let out = common::compile_and_run_box(
        PRIVATE_MEMBER,
        "Main",
        &[common::stdlib_jar(), jdk.clone()],
        Some(jdk.as_path()),
    );
    assert_eq!(out.as_deref(), Some("OK"));
}

const INTRINSIC: &str =
    "import kotlin.coroutines.intrinsics.suspendCoroutineUninterceptedOrReturn\n\
class IntrinsicPayload(val text: String)\n\
@JvmInline value class IntrinsicTicket(val payload: IntrinsicPayload)\n\
suspend fun direct(): IntrinsicTicket = suspendCoroutineUninterceptedOrReturn { IntrinsicTicket(IntrinsicPayload(\"x\")) }\n\
suspend fun read(): String = direct().payload.text\n";

/// `suspendCoroutineUninterceptedOrReturn`'s block hands back the box, so a function returning the
/// carrier unboxes it and keeps a continuation of its own; the continuation boxes the result again.
#[test]
fn an_intrinsic_suspension_point_is_not_forwarded_by_a_carrier_result() {
    for class in [
        "SuspendValueClassResultsKt$direct$1",
        "SuspendValueClassResultsKt$read$1",
    ] {
        expect_method_matches(
            INTRINSIC,
            class,
            "public final java.lang.Object invokeSuspend(java.lang.Object);",
        );
    }
}

const RESUMED_LATER: &str = r#"
import kotlin.coroutines.*
import kotlin.coroutines.intrinsics.*

class ResumePayload(val text: String)
class WrappedPayload(val text: String)
@JvmInline value class ResumeTicket(val payload: ResumePayload)
@JvmInline value class Wrapped(val payload: WrappedPayload)

var parked: Continuation<ResumeTicket>? = null
var parkedWrapped: Continuation<Wrapped>? = null
var out = ""

suspend fun park(tag: String = "O"): ResumeTicket = suspendCoroutineUninterceptedOrReturn { c ->
    parked = c
    COROUTINE_SUSPENDED
}

suspend fun parkWrapped(): Wrapped = suspendCoroutineUninterceptedOrReturn { c ->
    parkedWrapped = c
    COROUTINE_SUSPENDED
}

suspend fun <T> call(fn: suspend () -> T): T = fn()

fun builder(c: suspend () -> Unit) {
    c.startCoroutine(Continuation(EmptyCoroutineContext) { it.getOrThrow() })
}

fun box(): String {
    builder { out += park().payload.text }
    parked!!.resume(ResumeTicket(ResumePayload("O")))
    builder { out += call { parkWrapped() }.payload.text }
    parkedWrapped!!.resume(Wrapped(WrappedPayload("K")))
    return out
}
"#;

const NULLABLE_REFERENCE_CARRIER: &str = "@Suppress(\"RESULT_CLASS_IN_RETURN_TYPE\")\n\
suspend fun make(): Result<String> = Result.success(\"OK\")\n\
suspend fun read(): String = make().getOrThrow()\n\
@JvmInline value class Wrap(val v: Any?)\n\
suspend fun makeWrap(): Wrap = Wrap(\"OK\")\n\
suspend fun readWrap(): String = makeWrap().v as String\n\
@JvmInline value class Count(val n: Int)\n\
suspend fun makeCount(): Count = Count(1)\n\
suspend fun readCount(): Int = makeCount().n\n\
@Suppress(\"RESULT_CLASS_IN_RETURN_TYPE\")\n\
suspend fun makeNullable(): Result<String>? = Result.success(\"OK\")\n\
suspend fun readNullable(): String = makeNullable()!!.getOrThrow()\n";

/// A non-null `Result` and a value class over `Any?` return the carrier. The caller uses it on the
/// fast path and unboxes only the box a real resume delivers. A scalar, and a nullable `Result?`,
/// cross as the box on both paths.
#[test]
fn a_null_capable_reference_carrier_crosses_unboxed_until_resume() {
    for method in [
        "public static final java.lang.Object read(kotlin.coroutines.Continuation<? super java.lang.String>);",
        "public static final java.lang.Object readWrap(kotlin.coroutines.Continuation<? super java.lang.String>);",
        "public static final java.lang.Object readCount(kotlin.coroutines.Continuation<? super java.lang.Integer>);",
        "public static final java.lang.Object readNullable(kotlin.coroutines.Continuation<? super java.lang.String>);",
    ] {
        expect_method_matches(NULLABLE_REFERENCE_CARRIER, "SuspendValueClassResultsKt", method);
    }
}

const RESULT_SAM: &str = "import kotlin.coroutines.*\n\
fun interface ResultProvider {\n\
    suspend fun getResult(): Result<String>\n\
}\n\
var globalResult: String = \"<empty>\"\n\
suspend fun run() {\n\
    handleResult { Result.success(\"OK\") }\n\
}\n\
suspend fun handleResult(resultProvider: ResultProvider) {\n\
    globalResult = resultProvider.getResult().getOrThrow()\n\
}\n\
fun builder(c: suspend () -> Unit) {\n\
    c.startCoroutine(Continuation(EmptyCoroutineContext) { it.getOrThrow() })\n\
}\n\
fun box(): String {\n\
    builder { run() }\n\
    return globalResult\n\
}\n";

/// A fun-interface `suspend fun getResult(): Result<String>` that does not suspend returns
/// `constructor-impl`. The caller must not unbox that carrier as a `kotlin.Result`.
#[test]
fn a_fun_interface_result_completes_with_its_carrier() {
    common::expect_box_ok_with_stdlib(RESULT_SAM, "SuspendValueClassResults");
}

/// A value class resumed into a caller of `$default` unboxes it, and a suspend lambda hands its
/// value-class result over boxed, as every lambda does, so its continuation does not box it again.
#[test]
fn a_value_class_resumed_through_a_default_stub_and_a_lambda_is_boxed_once() {
    assert_eq!(
        common::compile_and_run_with_stdlib(RESUMED_LATER, "SuspendValueClassResults").as_deref(),
        Some("OK")
    );
}
