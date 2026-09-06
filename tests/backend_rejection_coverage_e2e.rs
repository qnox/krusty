//! BACKEND-REJECTION coverage: valid Kotlin that the front end accepts but the IR *backend* cleanly
//! DECLINES to lower, emitting a "not yet supported by the IR backend" style diagnostic (a non-zero
//! exit). The box corpus contains only SUPPORTED programs, so these bail branches
//! (`src/jvm/backend.rs`, `src/ir_lower.rs`, `src/jvm/suspend.rs`, `src/jvm/value_classes.rs`) are
//! otherwise never exercised. Each test drives the same front-end + JVM backend pipeline in-process
//! (front end passes; the backend bails) and asserts the compile is rejected.
//!
//! These are deliberately constructs krusty does NOT support yet — if one of them starts compiling,
//! the feature has landed and the test should be promoted to a real round-trip test elsewhere.

use super::common;

/// Compile `src` through the frontend and JVM backend in-process. Returns `true` only when the front
/// end accepts the source and the backend reaches one of its unsupported-feature exits. Returns
/// `true` (skip-clean) when the toolchain is absent, so the suite never fails spuriously on a machine
/// without the vendored kotlinc/JDK.
fn rejects(src: &str) -> bool {
    let stdlib = common::stdlib_jar();
    let jdk_modules = common::jdk_modules();
    common::backend_rejects_in_process(src, "S", &[stdlib], Some(jdk_modules.as_path()))
        .unwrap_or(false)
}

// (`UByte`/`UShort` used to be block-listed here. They are first-class `Ty` variants now — their
//  round-trip coverage lives in `feature_coverage_i_e2e`.)

// --- Mixed spread in a vararg call (`f(0, *a, 3)`) — lowered through the platform spread builder
//     (`IntSpreadBuilder` here), so it is ACCEPTED. ---

#[test]
fn mixed_spread_vararg_accepted() {
    assert!(!rejects(
        "fun f(vararg xs: Int) = xs.sum()\nfun main() { val a = intArrayOf(1, 2); println(f(0, *a, 3)) }\n"
    ));
}

// --- Delegated properties (`by`). ---

#[test]
fn delegated_property_observable_runs() {
    common::expect_box_ok_with_stdlib(
        "import kotlin.properties.Delegates\n\
         class C { var x: Int by Delegates.observable(0) { _, _, _ -> } }\n\
         fun box(): String { val c = C(); c.x = 5; return if (c.x == 5) \"OK\" else \"fail\" }\n",
        "ObservableDelegate",
    );
}

#[test]
fn delegated_property_lazy_accepted() {
    // `by lazy` now resolves its `getValue` through the classpath extension seam (LazyKt) — accepted.
    assert!(!rejects(
        "class C { val x: Int by lazy { 5 } }\nfun main() { println(C().x) }\n"
    ));
}

#[test]
fn delegated_property_map_accepted() {
    // `by map` resolves `Map.getValue` (a classpath extension in MapsKt) — accepted.
    assert!(!rejects(
        "class C(m: Map<String, Any?>) { val name: String by m }\n\
         fun main() { println(C(mapOf(\"name\" to \"a\")).name) }\n"
    ));
}

// --- Suspend-function shapes the state-machine builder declines (src/jvm/suspend.rs → lower_suspend
//     returns false; backend surfaces "this suspend-function shape is not yet supported"). Each shape
//     exercises a distinct un-handled control-flow construct around a suspension point. ---

#[test]
fn suspend_try_finally_runs() {
    let source = "import kotlin.coroutines.*\n\
         var log = \"\"\n\
         suspend fun d(value: String): String { log += value; return value }\n\
         suspend fun f(): String { try { d(\"O\") } finally { d(\"K\") }; return log }\n\
         fun box(): String {\n\
             var result = \"fail\"\n\
             val task: suspend () -> String = { f() }\n\
             task.startCoroutine(Continuation(EmptyCoroutineContext) { result = it.getOrThrow() })\n\
             return result\n\
         }\n";
    assert_eq!(
        common::compile_and_run_with_stdlib(source, "SuspendTryFinally").as_deref(),
        Some("OK")
    );
}

#[test]
fn suspend_try_catch_accepted() {
    assert!(!rejects(
        "suspend fun d(): Int = 1\n\
         suspend fun f(): Int { try { return d() } catch (e: Exception) { return d() } }\n"
    ));
}

#[test]
fn suspend_return_in_try_runs() {
    let source = "import kotlin.coroutines.*\n\
         var log = \"\"\n\
         suspend fun d(): String = \"O\"\n\
         suspend fun f(): String { try { return d() } finally { log += \"K\" } }\n\
         fun box(): String {\n\
             var result = \"fail\"\n\
             val task: suspend () -> String = { f() }\n\
             task.startCoroutine(Continuation(EmptyCoroutineContext) { result = it.getOrThrow() + log })\n\
             return result\n\
         }\n";
    assert_eq!(
        common::compile_and_run_with_stdlib(source, "SuspendReturnInTry").as_deref(),
        Some("OK")
    );
}

#[test]
fn suspend_try_as_expression_accepted() {
    // Previously rejected; the value-`try` desugar now rewrites the locally-BOUND form too
    // (`val x = try { … }` targets the bound local). Behavioral coverage:
    // `suspend_try_catch_shapes_e2e` (incl. the bound form's exception-type filtering).
    assert!(!rejects(
        "suspend fun d(): Int = 1\n\
         suspend fun f(): Int { val x = try { d() } catch (e: Exception) { 0 }; return x }\n"
    ));
}

// NOTE: a suspend call in a compound-assignment inside a `while`/`do-while`/`for` loop
// (`while (s < 3) { s += d() }`) is now LOWERED (the coroutine pass hoists the suspension to a temp).
// Promoted to a round-trip test in `suspend_loop_compound_assign_e2e.rs`.

#[test]
fn suspend_when_with_multiple_suspensions_runs() {
    let source = "import kotlin.coroutines.*\n\
         suspend fun d(value: String): String = value\n\
         suspend fun f(x: Int): String = when (x) { 0 -> d(\"fail\"); else -> d(\"O\") + d(\"K\") }\n\
         fun box(): String {\n\
             var result = \"fail\"\n\
             val task: suspend () -> String = { f(1) }\n\
             task.startCoroutine(Continuation(EmptyCoroutineContext) { result = it.getOrThrow() })\n\
             return result\n\
         }\n";
    assert_eq!(
        common::compile_and_run_with_stdlib(source, "SuspendWhenMultiple").as_deref(),
        Some("OK")
    );
}

// A suspend lambda body on a LAZY `Sequence.map` (returns `Sequence`, not `List`) must NOT be inlined
// into the `List`-materializing accumulate-loop desugar (that would hand back an `ArrayList` where the
// static type is `Sequence` → VerifyError). The `List`-result + `kotlin/collections` facade guard
// excludes it, so it falls through to the `FunctionN` path and the backend cleanly DECLINES. (kotlinc
// also rejects this outright — "suspension functions can only be called within coroutine body".)
#[test]
fn suspend_sequence_map_not_inlined_rejected() {
    assert!(rejects(
        "interface R { suspend fun g(x: Int): Int }\n\
         suspend fun f(s: Sequence<Int>, r: R): Sequence<Int> = s.map { r.g(it) }\n"
    ));
}

#[test]
fn suspend_safe_call_double_suspension_rejected() {
    assert!(rejects(
        "class Box { suspend fun d(): Int = 1 }\n\
         suspend fun f(b: Box?): Int { return (b?.d() ?: 0) + (b?.d() ?: 0) }\n"
    ));
}

#[test]
fn cross_file_suspend_generic_value_class_specialization_runs() {
    let sources = [
        (
            "Transform",
            "fun <T, R> T.mapResult(transform: suspend (T) -> R): suspend () -> R = { transform(this) }\n",
        ),
        (
            "Main",
            "import kotlin.coroutines.*\n\
             fun box(): String {\n\
                 var result = \"fail\"\n\
                 Result.success(\"OK\").mapResult { it.getOrThrow() }\n\
                     .startCoroutine(Continuation(EmptyCoroutineContext) { result = it.getOrThrow() })\n\
                 return result\n\
             }\n",
        ),
    ];
    assert_eq!(
        common::compile_and_run_files_with_stdlib(&sources).as_deref(),
        Some("OK"),
        "the checked cross-file suspend/value-class specialization must preserve its boxed generic boundary"
    );
}

#[test]
fn named_generic_value_class_operand_specialization_runs() {
    let source = "import kotlin.coroutines.*\n\
         fun <T, R> mapResult(\n\
             value: T,\n\
             other: String = \"x\",\n\
             transform: suspend (T) -> R\n\
         ): suspend () -> R = { transform(value) }\n\
         fun box(): String {\n\
             var result = \"fail\"\n\
             mapResult(\n\
                 transform = { it.getOrThrow() },\n\
                 value = Result.success(\"OK\")\n\
             ).startCoroutine(Continuation(EmptyCoroutineContext) { result = it.getOrThrow() })\n\
             return result\n\
         }\n";
    assert_eq!(
        common::compile_and_run_with_stdlib(source, "NamedGenericValueClass").as_deref(),
        Some("OK")
    );
}

#[test]
fn generic_member_value_class_operand_specialization_runs() {
    let source = "import kotlin.coroutines.*\n\
         class Mapper {\n\
             fun <T, R> mapResult(\n\
                 value: T,\n\
                 other: String = \"x\",\n\
                 transform: suspend (T) -> R\n\
             ): suspend () -> R = { transform(value) }\n\
         }\n\
         fun box(): String {\n\
             var result = \"fail\"\n\
             Mapper().mapResult(\n\
                 transform = { it.getOrThrow() },\n\
                 value = Result.success(\"OK\")\n\
             ).startCoroutine(Continuation(EmptyCoroutineContext) { result = it.getOrThrow() })\n\
             return result\n\
         }\n";
    assert_eq!(
        common::compile_and_run_with_stdlib(source, "MemberGenericValueClass").as_deref(),
        Some("OK")
    );
}

#[test]
fn owner_generic_member_value_class_operand_specialization_runs() {
    let source = "import kotlin.coroutines.*\n\
         class Mapper<T> {\n\
             fun <R> mapResult(\n\
                 value: T,\n\
                 transform: suspend (T) -> R\n\
             ): suspend () -> R = { transform(value) }\n\
         }\n\
         fun box(): String {\n\
             var result = \"fail\"\n\
             Mapper<Result<String>>().mapResult(\n\
                 value = Result.success(\"OK\"),\n\
                 transform = { it.getOrThrow() }\n\
             ).startCoroutine(Continuation(EmptyCoroutineContext) { result = it.getOrThrow() })\n\
             return result\n\
         }\n";
    assert_eq!(
        common::compile_and_run_with_stdlib(source, "OwnerGenericValueClass").as_deref(),
        Some("OK")
    );
}

#[test]
fn generic_member_extension_value_class_receiver_specialization_rejected() {
    // `rejects` used to answer true here only because the signature pass declined `trigger`
    // SILENTLY and the module produced neither classes nor diagnostics. The decline is reported
    // now, at the declaration, so the backend gate is never reached; pin the honest outcome.
    assert_eq!(
        common::front_end_diagnostics(
            "class Scope {\n\
                 fun <T, R> T.mapResult(\n\
                     transform: suspend (T) -> R\n\
                 ): suspend () -> R = { transform(this) }\n\
                 fun trigger() = Result.success(\"OK\").mapResult { \"OK\" }\n\
             }\n\
             fun box(): String = \"unreachable\"\n",
            std::slice::from_ref(&common::stdlib_jar()),
            Some(common::jdk_modules().as_path()),
        ),
        vec![
            "krusty: cannot infer the return type of 'trigger'; add an explicit return type"
                .to_string()
        ]
    );
}

#[test]
fn owner_generic_member_extension_value_class_receiver_specialization_rejected() {
    // Same as above: the reported decline replaces a silent no-output "rejection".
    assert_eq!(
        common::front_end_diagnostics(
            "class Scope<T> {\n\
                 fun <R> T.mapResult(\n\
                     transform: suspend (T) -> R\n\
                 ): suspend () -> R = { transform(this) }\n\
             }\n\
             fun trigger() = with(Scope<Result<String>>()) {\n\
                 Result.success(\"OK\").mapResult { \"OK\" }\n\
             }\n\
             fun box(): String = \"unreachable\"\n",
            std::slice::from_ref(&common::stdlib_jar()),
            Some(common::jdk_modules().as_path()),
        ),
        vec![
            "krusty: cannot infer the return type of 'trigger'; add an explicit return type"
                .to_string()
        ]
    );
}

#[test]
fn unused_explicit_value_class_type_argument_does_not_trigger_suspend_gate() {
    let source = "import kotlin.coroutines.*\n\
        fun <T> make(): suspend () -> String = { \"OK\" }\n\
        fun box(): String {\n\
            var result = \"fail\"\n\
            make<Result<String>>()\n\
                .startCoroutine(Continuation(EmptyCoroutineContext) { result = it.getOrThrow() })\n\
            return result\n\
        }\n";
    assert_eq!(
        common::compile_and_run_with_stdlib(source, "Main")
            .expect("unused explicit value-class formal must still lower"),
        "OK"
    );
}

#[test]
fn boxed_value_class_inside_container_does_not_trigger_suspend_gate() {
    let source = "import kotlin.coroutines.*\n\
        fun <T> make(values: List<T>): suspend () -> String = { \"OK\" }\n\
        fun box(): String {\n\
            var result = \"fail\"\n\
            make(listOf(Result.success(\"unused\")))\n\
                .startCoroutine(Continuation(EmptyCoroutineContext) { result = it.getOrThrow() })\n\
            return result\n\
        }\n";
    assert_eq!(
        common::compile_and_run_with_stdlib(source, "Main")
            .expect("boxed value-class element must not gate the container operand"),
        "OK"
    );
}

#[test]
fn unrelated_concrete_value_class_parameter_does_not_trigger_suspend_gate() {
    let source = "import kotlin.coroutines.*\n\
        fun <T> make(value: Result<String>): suspend () -> String = { value.getOrThrow() }\n\
        fun box(): String {\n\
            var result = \"fail\"\n\
            make<Int>(Result.success(\"OK\"))\n\
                .startCoroutine(Continuation(EmptyCoroutineContext) { result = it.getOrThrow() })\n\
            return result\n\
        }\n";
    assert_eq!(
        common::compile_and_run_with_stdlib(source, "Main")
            .expect("concrete value-class parameter does not bind the unused formal"),
        "OK"
    );
}

#[test]
fn cross_file_projected_generic_return_with_concrete_inference_is_accepted() {
    let (reference_code, reference_stderr) = common::kotlinc_source_result(
        "ProjectedGenericReturn",
        "fun <T> something(): T = \"OK\" as T\n\
         class Context<T>\n\
         fun <T> Any.decodeIn(typeFrom: Context<in T>): T = something()\n\
         fun <T> Any?.decodeOut(typeFrom: Context<out T>): T =\n\
             this?.decodeIn(typeFrom) ?: throw AssertionError()\n\
         fun box(): String = \"value\".decodeOut(Context<Any>()).toString()\n",
    );
    assert_eq!(
        reference_code, 0,
        "kotlinc rejected fixture: {reference_stderr}"
    );
    let sources = [
        (
            "Decode",
            "fun <T> something(): T = \"OK\" as T\n\
             class Context<T>\n\
             fun <T> Any.decodeIn(typeFrom: Context<in T>): T = something()\n",
        ),
        (
            "Main",
            "fun <T> Any?.decodeOut(typeFrom: Context<out T>): T =\n\
                 this?.decodeIn(typeFrom) ?: throw AssertionError()\n\
             fun box(): String = \"value\".decodeOut(Context<Any>()).toString()\n",
        ),
    ];
    let result = common::compile_and_run_files_with_stdlib(&sources);
    assert_eq!(
        result.as_deref(),
        Some("OK"),
        "frontend diagnostics: {:?}",
        common::module_front_end_diagnostics(&sources)
    );
}

#[test]
fn projected_generic_member_return_inference_matches_kotlinc() {
    const SOURCE: &str = "class Context<T>\n\
         fun <T> something(): T = Any() as T\n\
         class Decoder { fun <T> decodeIn(typeFrom: Context<in T>): T = something() }\n\
         fun <T> Decoder.decodeOut(typeFrom: Context<out T>): T = decodeIn(typeFrom)\n\
         fun box(): String = Decoder().decodeOut(Context<Any>()).toString()\n";
    let (reference_code, stderr) = common::kotlinc_source_result("ProjectedMemberReturn", SOURCE);
    assert_eq!(reference_code, 0, "kotlinc rejected fixture: {stderr}");
    let diagnostics = common::front_end_diagnostics_with_stdlib(SOURCE);
    assert!(
        diagnostics.is_empty(),
        "unexpected frontend diagnostics: {diagnostics:?}"
    );
}

#[test]
fn projected_generic_extension_receiver_with_concrete_inference_is_accepted() {
    let source = "class Context<T>\n\
         fun <T> something(): T = \"OK\" as T\n\
         fun <T> Context<in T>.decode(): T = something()\n\
         fun <T> Context<out T>.decodeOut(): T = decode()\n\
         fun box(): String = Context<Any>().decodeOut().toString()\n";
    assert_eq!(
        common::compile_and_run_with_stdlib(source, "ProjectedExtension").as_deref(),
        Some("OK")
    );
}

#[test]
fn suspend_ctor_arg_after_side_effect_accepted() {
    // Constructors now use the shared ordered-operand planner: `g()` is snapshotted before `d()`
    // is hoisted, preserving Kotlin's left-to-right evaluation instead of requiring a
    // constructor-specific rejection. Runtime order is pinned by `suspend_try_catch_shapes_e2e`.
    assert!(!rejects(
        "class S(val a: Int, val b: Int)\n\
         var log = 0\n\
         fun g(): Int { log += 1; return log }\n\
         suspend fun d(): Int = 1\n\
         suspend fun f(): Int { val s = S(g(), d()); return s.a + s.b }\n"
    ));
}

#[test]
fn suspend_ctor_single_arg_accepted() {
    // The one-argument case remains a direct instance of the same generic operand rule.
    assert!(!rejects(
        "class S(val a: Int)\n\
         suspend fun d(): Int = 1\n\
         suspend fun f(): Int { val s = S(d()); return s.a }\n"
    ));
}
