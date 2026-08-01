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
    let Some(stdlib) = common::stdlib_jar() else {
        return;
    };
    let Some(jdk) = common::jdk_modules() else {
        return;
    };
    assert!(
        common::compile_and_run_box_files(
            &[("Lib.kt", LIB), ("Main.kt", MAIN)],
            &[stdlib],
            Some(&jdk)
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
    let Some(stdlib) = common::stdlib_jar() else {
        return;
    };
    let Some(jdk) = common::jdk_modules() else {
        return;
    };
    assert!(
        common::compile_and_run_box_files(
            &[("Lib.kt", LIB), ("Main.kt", MAIN)],
            &[stdlib],
            Some(&jdk)
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
    let Some(stdlib) = common::stdlib_jar() else {
        return;
    };
    let Some(jdk) = common::jdk_modules() else {
        return;
    };
    assert!(
        common::compile_and_run_box_files(
            &[("Lib.kt", LIB), ("Main.kt", MAIN)],
            &[stdlib],
            Some(&jdk)
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
    let Some(stdlib) = common::stdlib_jar() else {
        return;
    };
    let Some(jdk) = common::jdk_modules() else {
        return;
    };
    assert!(
        common::compile_and_run_box_files(
            &[("Lib.kt", LIB), ("Main.kt", MAIN)],
            &[stdlib],
            Some(&jdk)
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
    let Some(stdlib) = common::stdlib_jar() else {
        return;
    };
    let Some(jdk) = common::jdk_modules() else {
        return;
    };
    assert!(
        common::compile_and_run_box_files(
            &[("Lib.kt", LIB), ("Main.kt", MAIN)],
            &[stdlib],
            Some(&jdk)
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

/// The `contracts/kt47168.kt` shape: an inline fn whose body carries a `contract { }` block
/// (erased, not a closure) and a TAIL value-return is safe standalone — it lowers + emits as a
/// facade static, so the cross-file call links. The `callsInPlace` contract is decoded but
/// unneeded for codegen here.
#[test]
fn contract_and_tail_return_inline_fun_called_cross_file() {
    const LIB: &str = "// OPT_IN: kotlin.contracts.ExperimentalContracts\n\
                       import kotlin.contracts.*\n\
                       inline fun foo(x: () -> String, y: () -> String): String {\n\
                       \x20   contract {\n\
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
