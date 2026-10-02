//! A `null` literal passed to a Java platform parameter whose substituted type is a FUNCTION
//! classifier. `AtomicReference<F>.getAndSet(F)` declares a platform `F!` parameter; with
//! `F := (String) -> String` the argument position is `Function1<String, String>!`, and Java's
//! flexible nullability admits `null`. Normalizing the nominal `FunctionN` classifier onto its
//! callable shape must keep the platform wrapper — collapsing `T!` to non-null rejects the literal.

use super::common;

/// The CallOnceFunction shape from the Kotlin monorepo's util.runtime module: a generic class whose
/// `AtomicReference` stores a function type, cleared with `getAndSet(null)`.
#[test]
fn null_to_a_platform_parameter_of_function_shape() {
    common::expect_box_same_as_kotlinc(
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
    common::expect_box_same_as_kotlinc(
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

/// An explicitly nullable function parameter admits `null`. This is the source-level counterpart of
/// the platform wrapper: both wrappers must survive classifier normalization.
#[test]
fn null_to_a_nullable_function_parameter() {
    common::expect_box_same_as_kotlinc(
        r#"
fun take(f: ((String) -> String)?): String = if (f == null) "OK" else f("x")

fun box(): String = take(null)
"#,
        "NullableFunctionParameterNull",
    );
}

/// A non-null function parameter rejects `null`. The diagnostic list, including file, line, column,
/// message, and order, must match kotlinc.
#[test]
fn null_to_a_non_null_function_parameter_is_rejected() {
    let result = common::compiler_diagnostics(
        &[(
            "Main.kt",
            r#"
fun take(f: (String) -> String): String = f("x")

fun box(): String = take(null)
"#,
        )],
        &[],
    );
    common::expect_identical_rejection(&result, "null passed to a non-null function parameter");
}
