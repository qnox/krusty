//! Cross-file calls to top-level `inline` functions: the defining file lowers + emits the inline
//! function as a facade static (kotlinc's `public static synthetic` shape), so a call from ANOTHER
//! source file of the same module links and runs. Same-file call sites still splice the body.

use super::common;

/// The `boxingOptimization/nullCheck.kt` shape from the Kotlin box corpus — the top survey bucket
/// (`emit: emit_all bailed`): an inline-only `lib.kt` emitted nothing, so the cross-file call in
/// `main.kt` had no callee to link against.
#[test]
fn generic_inline_fun_called_cross_file() {
    const LIB: &str = "inline fun <R, T> foo(x: R?, y: R?, block: (R?) -> T): T {\n\
                       \x20   if (x == null) {\n\
                       \x20       return block(x)\n\
                       \x20   } else {\n\
                       \x20       return block(y)\n\
                       \x20   }\n\
                       }\n";
    const MAIN: &str = "fun box(): String {\n\
                        \x20   val r = foo(1, 2) { x -> if (x != null) 3 else 4 }\n\
                        \x20   return if (r == 3) \"OK\" else \"fail: $r\"\n\
                        }\n";
    common::expect_box_ok_files_with_stdlib(
        &[("Lib.kt", LIB), ("Main.kt", MAIN)],
        "cross_file_generic_inline",
    );
}

/// The minimal shape: a non-generic top-level inline function called from another file.
#[test]
fn plain_inline_fun_called_cross_file() {
    const LIB: &str = "inline fun twice(x: Int, block: (Int) -> Int): Int = block(block(x))\n";
    const MAIN: &str = "fun box(): String {\n\
                        \x20   val r = twice(3) { it + 1 }\n\
                        \x20   return if (r == 5) \"OK\" else \"fail: $r\"\n\
                        }\n";
    common::expect_box_ok_files_with_stdlib(
        &[("Lib.kt", LIB), ("Main.kt", MAIN)],
        "cross_file_plain_inline",
    );
}

/// A non-local `return` in the lambda is legal ONLY when the callee is spliced. The cross-file
/// `invokestatic` fallback must keep the honest backend rejection — never link a broken closure.
#[test]
fn non_local_return_in_cross_file_inline_lambda_still_rejects() {
    const LIB: &str = "inline fun untilDone(x: Int, block: (Int) -> Unit): Int {\n\
                       \x20   block(x)\n\
                       \x20   return x + 1\n\
                       }\n";
    const MAIN: &str = "fun box(): String {\n\
                        \x20   var hit = 0\n\
                        \x20   untilDone(4) { hit = it; if (it == 4) return \"early\" }\n\
                        \x20   return if (hit == 4) \"OK\" else \"fail: $hit\"\n\
                        }\n";
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    assert!(
        common::compile_and_run_box_files(
            &[("Lib.kt", LIB), ("Main.kt", MAIN)],
            &[stdlib],
            Some(jdk.as_path())
        )
        .is_none(),
        "cross-file inline call with a non-local return must be rejected, never emitted"
    );
}

/// An inline function whose SIGNATURE mentions a value class needs mangling/erasure a cross-file
/// `invokestatic` doesn't apply — it stays splice-only, and the cross-file call must REJECT
/// (a fall-through once misread this call as a constructor of its result type — a miscompile).
#[test]
fn value_class_signature_inline_fun_cross_file_still_rejects() {
    const LIB: &str = "inline fun new(init: (Z) -> Unit): Z = Z(42)\n\
                       @JvmInline\n\
                       value class Z(val value: Int)\n";
    const MAIN: &str = "fun box(): String =\n\
                        \x20   if (new(fun(z: Z) {}).value == 42) \"OK\" else \"Fail\"\n";
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    assert!(
        common::compile_and_run_box_files(
            &[("Lib.kt", LIB), ("Main.kt", MAIN)],
            &[stdlib],
            Some(jdk.as_path())
        )
        .is_none(),
        "cross-file call to a value-class-signature inline fun must be rejected, never emitted"
    );
}

/// An inline function whose body constructs a sealed class's nested subclass: the standalone body
/// resolves the nested constructor in its own file's scope (`nullCheckOptimization/trivialInstanceOf`).
#[test]
fn inline_fun_constructing_sealed_nested_class_cross_file() {
    const LIB: &str = "sealed class A {\n\
                       \x20   class B : A()\n\
                       \x20   class C : A()\n\
                       }\n\
                       inline fun foo(): A = A.B()\n";
    const MAIN: &str = "fun box(): String {\n\
                        \x20   val a: A = foo()\n\
                        \x20   val b: Boolean\n\
                        \x20   when (a) {\n\
                        \x20       is A.B -> b = true\n\
                        \x20       is A.C -> b = false\n\
                        \x20   }\n\
                        \x20   return if (b) \"OK\" else \"FAIL\"\n\
                        }\n";
    common::expect_box_ok_files_with_stdlib(
        &[("Lib.kt", LIB), ("Main.kt", MAIN)],
        "cross_file_inline_sealed_nested",
    );
}

/// A lambda that MUTATES a captured local of the caller is only sound when spliced (the checker
/// analysed its captures for splicing — a closure would mutate a copy). The cross-file call must
/// reject. Writes to top-level properties stay field accesses and remain fine.
#[test]
fn mutable_capture_in_cross_file_inline_lambda_still_rejects() {
    const LIB: &str = "inline fun foo(x: Int, action: (Int) -> Unit) = action(x)\n";
    const MAIN: &str = "fun box(): String {\n\
                        \x20   var x = 23\n\
                        \x20   foo(x) { x++ }\n\
                        \x20   return if (x == 24) \"OK\" else \"fail: $x\"\n\
                        }\n";
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    assert!(
        common::compile_and_run_box_files(
            &[("Lib.kt", LIB), ("Main.kt", MAIN)],
            &[stdlib],
            Some(jdk.as_path())
        )
        .is_none(),
        "cross-file inline call with a mutating capture must be rejected, never emitted"
    );
}

/// A callable-reference argument's adapted/bound-reference lowering assumes splicing — the
/// cross-file call must reject (`callableReference/kt49526`).
#[test]
fn callable_ref_arg_in_cross_file_inline_call_still_rejects() {
    const LIB: &str = "inline fun <T> useRef(value: T, f: (T) -> Boolean) = f(value)\n";
    const MAIN: &str = "fun box(): String {\n\
                        \x20   val chars = listOf('a') + \"-\"\n\
                        \x20   return if (useRef('a', chars::contains)) \"OK\" else \"Fail\"\n\
                        }\n";
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    assert!(
        common::compile_and_run_box_files(
            &[("Lib.kt", LIB), ("Main.kt", MAIN)],
            &[stdlib],
            Some(jdk.as_path())
        )
        .is_none(),
        "cross-file inline call with a callable-ref argument must be rejected, never emitted"
    );
}

/// An inline body carrying splice-only control flow (`try`/`finally` here) is analysed by the
/// checker with splice assumptions, so it is never emitted standalone — the call rejects.
#[test]
fn try_body_inline_fun_cross_file_still_rejects() {
    const LIB: &str = "fun zap(s: String) = s\n\
                       inline fun tryZap(string: String, fn: (String) -> String) =\n\
                       \x20   fn(try { zap(string) } finally { zap(string) })\n";
    const MAIN: &str = "fun box(): String = tryZap(\"OK\") { it }\n";
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    assert!(
        common::compile_and_run_box_files(
            &[("Lib.kt", LIB), ("Main.kt", MAIN)],
            &[stdlib],
            Some(jdk.as_path())
        )
        .is_none(),
        "cross-file call to a try-bodied inline fun must be rejected, never emitted"
    );
}

/// Same-file call sites keep splicing (non-local return only works when the body is inlined).
#[test]
fn same_file_inline_still_splices() {
    const SRC: &str = "inline fun untilDone(x: Int, block: (Int) -> Unit): Int {\n\
                       \x20   block(x)\n\
                       \x20   return x + 1\n\
                       }\n\
                       fun box(): String {\n\
                       \x20   var hit = 0\n\
                       \x20   val r = untilDone(4) { hit = it; return@untilDone }\n\
                       \x20   return if (r == 5 && hit == 4) \"OK\" else \"fail: $r/$hit\"\n\
                       }\n";
    common::expect_box_ok_with_stdlib(SRC, "same_file_inline_splice_kept");
}

/// The `contracts/kt47168.kt` shape: an inline fn whose body carries an ALIASED contract intrinsic
/// (erased, not a closure) and a tail value-return is safe standalone — it lowers + emits as a
/// facade static, so the cross-file call links. Resolving the alias to semantic callable identity
/// keeps inline eligibility consistent with checker erasure; the effects themselves are unneeded
/// for codegen here.
#[test]
fn contract_and_tail_return_inline_fun_called_cross_file() {
    const LIB: &str = "// OPT_IN: kotlin.contracts.ExperimentalContracts\n\
                       import kotlin.contracts.InvocationKind\n\
                       import kotlin.contracts.contract as declareContract\n\
                       inline fun foo(x: () -> String, y: () -> String): String {\n\
                       \x20   declareContract {\n\
                       \x20       callsInPlace(x, InvocationKind.EXACTLY_ONCE)\n\
                       \x20       callsInPlace(y, InvocationKind.EXACTLY_ONCE)\n\
                       \x20   }\n\
                       \x20   return x() + y()\n\
                       }\n";
    const MAIN: &str = "fun box(): String {\n\
                        \x20   val y = { \"K\" }\n\
                        \x20   return foo({ \"O\" }, y)\n\
                        }\n";
    common::expect_box_ok_files_with_stdlib(
        &[("Lib.kt", LIB), ("Main.kt", MAIN)],
        "cross_file_contract_tail_return_inline",
    );
}

/// A same-named declaration in an UNRELATED package must not globally disable contract erasure or
/// facade emission. The federated resolver applies the defining file's package/import scope first;
/// the unrelated source remains present in the module symbol table but is not a candidate here.
#[test]
fn unrelated_package_contract_name_does_not_shadow_intrinsic() {
    const UNRELATED: &str = "package unrelated\n\
                             fun contract(block: () -> Unit) { block() }\n";
    const LIB: &str = "// OPT_IN: kotlin.contracts.ExperimentalContracts\n\
                       package target\n\
                       import kotlin.contracts.*\n\
                       inline fun combine(x: () -> String, y: () -> String): String {\n\
                       \x20   contract {\n\
                       \x20       callsInPlace(x, InvocationKind.EXACTLY_ONCE)\n\
                       \x20       callsInPlace(y, InvocationKind.EXACTLY_ONCE)\n\
                       \x20   }\n\
                       \x20   return x() + y()\n\
                       }\n";
    const MAIN: &str = "package target\n\
                        fun box(): String = combine({ \"O\" }, { \"K\" })\n";
    common::expect_box_ok_files_with_stdlib(
        &[
            ("Unrelated.kt", UNRELATED),
            ("Lib.kt", LIB),
            ("Main.kt", MAIN),
        ],
        "cross_file_contract_scope_isolation",
    );
}

/// The inverse precedence case: a LOCAL function shadows an imported intrinsic inside this inline
/// body. Its directly passed lambda is safe in an emitted body, but it must execute normally rather
/// than be erased as contract DSL. Requiring the runtime result proves facade pre-check and checker
/// erasure agree on lexical identity; merely accepting the source would not catch accidental erasure.
#[test]
fn local_contract_shadow_executes_in_cross_file_inline_facade() {
    const LIB: &str = "import kotlin.contracts.*\n\
                       var localContractResult = \"\"\n\
                       inline fun runLocalContract(): String {\n\
                       \x20   fun contract(block: () -> Unit) { block() }\n\
                       \x20   contract { localContractResult = \"OK\" }\n\
                       \x20   return localContractResult\n\
                       }\n";
    const MAIN: &str = "fun box(): String = runLocalContract()\n";
    common::expect_box_ok_files_with_stdlib(
        &[("Lib.kt", LIB), ("Main.kt", MAIN)],
        "cross_file_local_contract_shadow_executes",
    );
}

/// A function-typed VARIABLE argument to a cross-file inline facade static: the variable's value
/// is a real closure, read at the call site and `invoke`d by the static like any other object.
#[test]
fn fun_typed_variable_arg_to_cross_file_inline() {
    const LIB: &str =
        "inline fun applyBoth(x: () -> String, y: () -> String): String = x() + y()\n";
    const MAIN: &str = "fun box(): String {\n\
                        \x20   val a = { \"O\" }\n\
                        \x20   val b = { \"K\" }\n\
                        \x20   return applyBoth(a, b)\n\
                        }\n";
    common::expect_box_ok_files_with_stdlib(
        &[("Lib.kt", LIB), ("Main.kt", MAIN)],
        "cross_file_inline_fun_typed_var_arg",
    );
}

/// The `contracts/kt47300.kt` shape: inline GENERIC EXTENSIONS with function-typed parameters,
/// one body passing a lambda literal to another inline fn (spliced at emission), the other
/// carrying a `contract { }` block and a tail value-return — all safe standalone, so they emit
/// as facade statics and the cross-file call links.
#[test]
fn generic_inline_extension_with_fun_params_called_cross_file() {
    const LIB: &str = "// OPT_IN: kotlin.contracts.ExperimentalContracts\n\
                       import kotlin.contracts.ExperimentalContracts\n\
                       import kotlin.contracts.InvocationKind\n\
                       import kotlin.contracts.contract\n\
                       data class Content<out T>(val value: T)\n\
                       fun <T> content(value: T) = Content(value)\n\
                       @ExperimentalContracts\n\
                       inline fun <R, T : R> Content<T>.getOrElse(\n\
                       \x20   onException: (exception: Exception) -> R,\n\
                       ): R = fold({ it }, onException)\n\
                       @ExperimentalContracts\n\
                       inline fun <R, T> Content<T>.fold(\n\
                       \x20   onContent: (value: T) -> R,\n\
                       \x20   onException: (exception: Exception) -> R,\n\
                       ): R {\n\
                       \x20   contract {\n\
                       \x20       callsInPlace(onContent, InvocationKind.AT_MOST_ONCE)\n\
                       \x20       callsInPlace(onException, InvocationKind.AT_MOST_ONCE)\n\
                       \x20   }\n\
                       \x20   return onContent(value)\n\
                       }\n";
    const MAIN: &str = "import kotlin.contracts.ExperimentalContracts\n\
                        @ExperimentalContracts\n\
                        fun box(): String {\n\
                        \x20   val t = content(1).getOrElse { 2 }\n\
                        \x20   if (t != 1) return \"Failed: $t\"\n\
                        \x20   return \"OK\"\n\
                        }\n";
    common::expect_box_ok_files_with_stdlib(
        &[("Lib.kt", LIB), ("Main.kt", MAIN)],
        "cross_file_generic_inline_extension",
    );
}

/// A lambda PARAM passed through as another inline fn's lambda argument: inside the emitted
/// static the parameter is a runtime closure value — the splice invokes it, never re-splices it.
#[test]
fn inline_extension_lambda_param_forwarded_through_splice() {
    const LIB: &str = "inline fun <T, R> T.mapTwice(f: (T) -> R, g: (R) -> R): R = g(f(this))\n\
                       inline fun <T, R> T.runMapped(f: (T) -> R): R = mapTwice(f, { it })\n";
    const MAIN: &str = "fun box(): String {\n\
                        \x20   val r = 20.runMapped { it + 1 }\n\
                        \x20   return if (r == 21) \"OK\" else \"fail: $r\"\n\
                        }\n";
    common::expect_box_ok_files_with_stdlib(
        &[("Lib.kt", LIB), ("Main.kt", MAIN)],
        "cross_file_inline_ext_forward_lambda",
    );
}

/// Guard against over-widening: an inline fn whose body STORES a lambda (closure synthesis with
/// splice assumptions) stays splice-only — the cross-file call still rejects.
#[test]
fn stored_lambda_body_inline_fun_cross_file_still_rejects() {
    const LIB: &str = "inline fun makeAndCall(): String {\n\
                       \x20   val f = { \"OK\" }\n\
                       \x20   return f()\n\
                       }\n";
    const MAIN: &str = "fun box(): String = makeAndCall()\n";
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    assert!(
        common::compile_and_run_box_files(
            &[("Lib.kt", LIB), ("Main.kt", MAIN)],
            &[stdlib],
            Some(jdk.as_path())
        )
        .is_none(),
        "cross-file call to a stored-lambda-body inline fun must be rejected, never emitted"
    );
}

/// Guard: a lambda argument carrying a `return` (a non-local return through the inline frame)
/// keeps the callee splice-only — the framing only holds when spliced into a caller, so the
/// cross-file call still rejects rather than emit a broken state (storeStackBeforeInline/
/// unreachableMarker.kt).
#[test]
fn return_in_lambda_arg_body_inline_fun_cross_file_still_rejects() {
    const LIB: &str = "inline fun bar(block: () -> String): String {\n\
                       \x20   return block()\n\
                       }\n\
                       inline fun bar2(): String {\n\
                       \x20   return bar { return \"def\" }\n\
                       }\n";
    const MAIN: &str = "fun box(): String = bar2()\n";
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    assert!(
        common::compile_and_run_box_files(
            &[("Lib.kt", LIB), ("Main.kt", MAIN)],
            &[stdlib],
            Some(jdk.as_path())
        )
        .is_none(),
        "cross-file call to a non-local-return-lambda inline fun must be rejected, never emitted"
    );
}

/// Guard: a value-class RECEIVER on an inline extension with a function-typed parameter (so the
/// cross-module `has_callable_inline_extension_body` path doesn't apply) screens out — a
/// cross-file `invokestatic` applies no value-class mangling/erasure to arg0.
#[test]
fn value_class_receiver_inline_extension_cross_file_still_rejects() {
    const LIB: &str = "@JvmInline\n\
                       value class Z(val value: Int)\n\
                       inline fun Z.transform(f: (Int) -> Int): Int = f(value)\n";
    const MAIN: &str =
        "fun box(): String = if (Z(21).transform { it * 2 } == 42) \"OK\" else \"fail\"\n";
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    assert!(
        common::compile_and_run_box_files(
            &[("Lib.kt", LIB), ("Main.kt", MAIN)],
            &[stdlib],
            Some(jdk.as_path())
        )
        .is_none(),
        "cross-file call to a value-class-receiver inline extension must be rejected, never emitted"
    );
}

/// Guard: a REIFIED inline extension with a function-typed parameter (again outside the
/// cross-module path) specializes per call site — it stays splice-only and the cross-file call
/// still rejects.
#[test]
fn reified_inline_extension_cross_file_still_rejects() {
    const LIB: &str = "inline fun <reified T> T.check(f: () -> Unit): Boolean = this is T\n";
    const MAIN: &str = "fun box(): String = if (1.check { }) \"OK\" else \"fail\"\n";
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    assert!(
        common::compile_and_run_box_files(
            &[("Lib.kt", LIB), ("Main.kt", MAIN)],
            &[stdlib],
            Some(jdk.as_path())
        )
        .is_none(),
        "cross-file call to a reified inline extension must be rejected, never emitted"
    );
}

/// A cross-file `suspend` extension is called through its real CPS entry point, never spliced:
/// `inline` on the declaration does not change the sibling-file ABI (kotlinc emits the same
/// `plusOne(int, Continuation)` method for both, plus a private `$$forInline` copy it splices only
/// within the declaring compilation). The call site must therefore thread the caller's continuation
/// and take the erased result — the shape a non-extension cross-file suspend call already had.
#[test]
fn suspend_inline_extension_cross_file_executes() {
    const LIB: &str = "inline suspend fun Int.plusOne(): Int = this + 1\n";
    common::expect_box_ok_files_with_stdlib(
        &[("Lib.kt", LIB), ("Main.kt", SUSPEND_EXT_MAIN)],
        "suspend_inline_extension_cross_file",
    );
}

/// The non-`inline` sibling of the case above: same call-site threading, same answer. Both shapes
/// share the `ResolvedCall::ModuleExtension` lowering path, so they are guarded together.
#[test]
fn suspend_extension_cross_file_executes() {
    const LIB: &str = "suspend fun Int.plusOne(): Int = this + 1\n";
    common::expect_box_ok_files_with_stdlib(
        &[("Lib.kt", LIB), ("Main.kt", SUSPEND_EXT_MAIN)],
        "suspend_extension_cross_file",
    );
}

/// A cross-file suspend extension that really SUSPENDS (its body awaits another suspend call)
/// resumes into the caller's state machine — the resumed value must still reach the assignment.
#[test]
fn suspend_extension_cross_file_with_suspension_point_executes() {
    const LIB: &str = "suspend fun twice(x: Int): Int = x * 2\n\
                       suspend fun Int.plusOne(): Int = twice(this) - this + 1\n";
    common::expect_box_ok_files_with_stdlib(
        &[("Lib.kt", LIB), ("Main.kt", SUSPEND_EXT_MAIN)],
        "suspend_extension_cross_file_suspension_point",
    );
}

/// Drives `Int.plusOne()` from a `suspend { … }` lambda: the assignment it performs is observable
/// only if the call threaded a continuation, since `EC` swallows a failed resume.
const SUSPEND_EXT_MAIN: &str = "import kotlin.coroutines.*\n\
                                class EC : Continuation<Unit> {\n\
                                \x20   override val context: CoroutineContext = EmptyCoroutineContext\n\
                                \x20   override fun resumeWith(result: Result<Unit>) {}\n\
                                }\n\
                                fun box(): String {\n\
                                \x20   var r = 0\n\
                                \x20   suspend { r = 1.plusOne() }.startCoroutine(EC())\n\
                                \x20   return if (r == 2) \"OK\" else \"fail\"\n\
                                }\n";

/// A `::ref` to a sibling-file inline fn that is NOT emitted (reified — it specializes per call
/// site) used to decline silently: the reference fell through to unrelated overloads or the file
/// died with the generic backend error. The checker now names the real problem at the reference.
#[test]
fn cross_file_ref_to_unemitted_inline_fn_names_the_reason() {
    const LIB: &str = "inline fun <reified T> tag(): String = \"t\"\n";
    const MAIN: &str = "fun box(): String {\n\
                        \x20   val f: () -> String = ::tag\n\
                        \x20   return f()\n\
                        }\n";
    let Some(diags) = common::module_front_end_diagnostics(&[("Lib.kt", LIB), ("Main.kt", MAIN)])
    else {
        return;
    };
    assert!(
        diags.iter().any(|d| d
            .contains("cannot reference 'tag': the inline function is not emitted as a callable")),
        "expected the unemitted-inline diagnostic, got: {diags:?}"
    );
}

/// A bound reference must follow the same declaration-keyed facade decision as a direct call.
/// Generic inline extensions with safe bodies are now emitted even when they accept a function;
/// executing the reference verifies that resolution and lowering agree on the static receiver ABI.
#[test]
fn cross_file_bound_ref_to_emitted_inline_extension_executes() {
    const LIB: &str = "inline fun <T> T.tag(f: () -> String): String = f()\n";
    const MAIN: &str = "fun box(): String {\n\
                        \x20   val g: (() -> String) -> String = \"x\"::tag\n\
                        \x20   return g { \"OK\" }\n\
                        }\n";
    common::expect_box_ok_files_with_stdlib(
        &[("Lib.kt", LIB), ("Main.kt", MAIN)],
        "cross_file_bound_ref_emitted_inline_extension",
    );
}

/// The UNBOUND extension-reference route (`Type::extension`) has its own candidate selection before
/// it reaches the shared facade outcome. Execute it separately from `value::extension` so both
/// syntax forms prove that an emitted extension exposes its receiver as argument zero.
#[test]
fn cross_file_unbound_ref_to_emitted_inline_extension_executes() {
    const LIB: &str = "inline fun <T> T.tag(f: () -> String): String = f()\n";
    const MAIN: &str = "fun box(): String {\n\
                        \x20   val g: (String, () -> String) -> String = String::tag\n\
                        \x20   return g(\"x\") { \"OK\" }\n\
                        }\n";
    common::expect_box_ok_files_with_stdlib(
        &[("Lib.kt", LIB), ("Main.kt", MAIN)],
        "cross_file_unbound_ref_emitted_inline_extension",
    );
}

/// Guard against over-firing: references to fns that ARE facade-emitted (a plain fn, and an
/// inline fn eligible for the facade-static path) still resolve clean cross-file.
#[test]
fn cross_file_ref_to_emitted_fns_stays_clean() {
    const LIB: &str = "fun plain(x: Int): Int = x + 1\n\
                       inline fun twice(x: Int, block: (Int) -> Int): Int = block(block(x))\n";
    const MAIN: &str = "fun box(): String {\n\
                        \x20   val p: (Int) -> Int = ::plain\n\
                        \x20   val t: (Int, (Int) -> Int) -> Int = ::twice\n\
                        \x20   return \"OK\"\n\
                        }\n";
    let Some(diags) = common::module_front_end_diagnostics(&[("Lib.kt", LIB), ("Main.kt", MAIN)])
    else {
        return;
    };
    assert!(diags.is_empty(), "expected no diagnostics, got: {diags:?}");
}

/// A checker-only pipeline (the LSP path — no `prepare_module_symbols` registration) must NOT
/// report the unemitted-inline diagnostic for a cross-file reference to a plain fn: without
/// registration data the reference declines silently and types through the fallbacks, as before.
#[test]
fn checker_only_pipeline_cross_file_ref_to_plain_fn_stays_clean() {
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let diags = common::front_end_diagnostics_files(
        &[
            "fun plain(x: Int): Int = x + 1\n",
            "fun box(): String {\n\
             \x20   val p: (Int) -> Int = ::plain\n\
             \x20   return \"OK\"\n\
             }\n",
        ],
        &[stdlib],
        Some(jdk.as_path()),
    );
    assert!(diags.is_empty(), "expected no diagnostics, got: {diags:?}");
}

/// The ADAPTED-reference form (default-parameter adaptation): `::tag` passed to a
/// function-typed parameter of smaller arity resolves through `select_adapted_source_ref`,
/// which must name the same reason for an unemitted sibling inline fn rather than decline
/// silently.
#[test]
fn cross_file_adapted_ref_to_unemitted_inline_fn_names_the_reason() {
    const LIB: &str = "inline fun <reified T> tag(x: String, y: Char = 'K'): String = x + y\n";
    const MAIN: &str = "fun <T, U> call(f: (T) -> U, x: T): U = f(x)\n\
                        fun box(): String = call(::tag, \"O\")\n";
    let Some(diags) = common::module_front_end_diagnostics(&[("Lib.kt", LIB), ("Main.kt", MAIN)])
    else {
        return;
    };
    assert!(
        diags.iter().any(|d| d
            .contains("cannot reference 'tag': the inline function is not emitted as a callable")),
        "expected the unemitted-inline diagnostic, got: {diags:?}"
    );
}
