//! A lambda literal fits a NULLABLE function-typed parameter.
//!
//! Overload applicability asks whether a parameter can host a lambda literal directly or has to go
//! through a SAM conversion, and it asked that of the parameter's declared type rather than its
//! shape. A nullable function type is not a functional interface, so `((Int) -> Int)?` took the SAM
//! branch, matched nothing, and the candidate was dropped — leaving the call unselected.
//!
//! When such a call is the initializer of an INFERRED declaration the failure is silent: signature
//! evaluation fails, the module falls into diagnostic recovery, and recovery has no expression to
//! blame because the body itself is well typed. Whole modules therefore compiled to nothing while
//! reporting success. Every sibling shape test in this path already reads the parameter through
//! `non_null`.

use super::common;

/// The failing shape: a lambda literal for a nullable function-typed constructor parameter, in a
/// call whose result type is inferred.
#[test]
fn a_lambda_literal_fits_a_nullable_function_parameter() {
    const MAIN: &str = "class Callbacks(val onEach: ((Int) -> Int)?)\n\
fun build() = Callbacks({ value -> value * 2 })\n\
fun box(): String = if (build().onEach?.invoke(21) == 42) \"OK\" else \"FAIL\"\n";
    common::expect_box_ok_files_with_stdlib(&[("Main.kt", MAIN)], "nullable_function_parameter");
}

/// The same parameter carrying a DEFAULT, with a later parameter omitted — the corpus shape. The
/// selected constructor must still map the supplied lambda to its own parameter.
#[test]
fn a_defaulted_nullable_function_parameter_still_selects() {
    const MAIN: &str = "class Callbacks(\n\
\x20   val first: (Int) -> Int,\n\
\x20   val second: ((Int) -> Int)? = null,\n\
\x20   val third: ((Int) -> Int)? = null,\n\
)\n\
fun build() = Callbacks(first = { it + 1 }, second = { it + 2 })\n\
fun box(): String {\n\
\x20   val callbacks = build()\n\
\x20   if (callbacks.first(1) != 2) return \"FAIL: first\"\n\
\x20   if (callbacks.second?.invoke(1) != 3) return \"FAIL: second\"\n\
\x20   if (callbacks.third != null) return \"FAIL: third\"\n\
\x20   return \"OK\"\n\
}\n";
    common::expect_box_ok_files_with_stdlib(&[("Main.kt", MAIN)], "defaulted_nullable_function");
}

/// A nullable SAM parameter keeps taking the SAM branch: the shape test decides which branch runs,
/// and widening it must not turn a functional interface into a plain function type.
#[test]
fn a_nullable_sam_parameter_still_converts() {
    const MAIN: &str = "fun interface Mapper {\n\
\x20   fun map(value: Int): Int\n\
}\n\
class Callbacks(val mapper: Mapper?)\n\
fun build() = Callbacks({ value -> value * 3 })\n\
fun box(): String = if (build().mapper?.map(14) == 42) \"OK\" else \"FAIL\"\n";
    common::expect_box_ok_files_with_stdlib(&[("Main.kt", MAIN)], "nullable_sam_parameter");
}

/// A nullable parameter that is NOT function-shaped still rejects a lambda literal, so the widened
/// shape test did not become a wildcard. Both compilers' complete output is asserted.
#[test]
fn a_nullable_non_function_parameter_still_rejects_a_lambda() {
    const MAIN: &str = "class Callbacks(val label: String?)\n\
fun build() = Callbacks({ value: Int -> value })\n";
    let result = common::compiler_diagnostics(&[("Main.kt", MAIN)], &[]);
    assert_ne!(
        result.reference_code, 0,
        "kotlinc must reject a lambda for a String? parameter: {}",
        result.reference_stderr
    );
    assert_ne!(
        result.krusty_code, 0,
        "krusty accepted a lambda for a String? parameter: {}{}",
        result.krusty_stdout, result.krusty_stderr
    );
}
