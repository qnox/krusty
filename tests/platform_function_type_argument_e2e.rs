//! A `null` literal passed to a Java platform parameter whose substituted type is a FUNCTION
//! classifier. `AtomicReference<F>.getAndSet(F)` declares a platform `F!` parameter; with
//! `F := (String) -> String` the argument position is `Function1<String, String>!`, and Java's
//! flexible nullability admits `null`. Normalizing the nominal `FunctionN` classifier onto its
//! callable shape must keep the platform wrapper — collapsing `T!` to non-null rejects the literal.

use super::common;

fn run_box(src: &str, stem: &str) {
    let Some(out) = common::compile_and_run_with_stdlib(src, stem) else {
        panic!("{stem}: expected the box to compile and run");
    };
    assert_eq!(out, "OK", "{stem}");
}

/// The CallOnceFunction shape from the Kotlin monorepo's util.runtime module: a generic class whose
/// `AtomicReference` stores a function type, cleared with `getAndSet(null)`.
#[test]
fn null_to_a_platform_parameter_of_function_shape() {
    run_box(
        r#"
import java.util.concurrent.atomic.AtomicReference

class CallOnceFunction<F, T : Any> {
    private val functionRef = AtomicReference<(F) -> T>()

    fun set(function: (F) -> T) {
        functionRef.getAndSet(null)
        functionRef.set(function)
    }

    fun call(arg: F): T = functionRef.get()(arg)
}

fun box(): String {
    val callOnce = CallOnceFunction<String, String>()
    callOnce.set({ it + "OK" })
    return callOnce.call("")
}
"#,
        "PlatformFunctionShapeNull",
    );
}

/// Control: the same Java platform parameter over a NON-function type argument already admitted
/// `null` before the normalization fix.
#[test]
fn null_to_a_platform_parameter_of_nominal_shape() {
    run_box(
        r#"
import java.util.concurrent.atomic.AtomicReference

fun box(): String {
    val ref = AtomicReference<String>()
    ref.set("x")
    ref.getAndSet(null)
    ref.set("OK")
    return ref.get()
}
"#,
        "PlatformNominalShapeNull",
    );
}
