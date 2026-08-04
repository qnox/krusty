//! A call to a CLASSPATH function that NAMES a parameter and OMITS an earlier defaulted one
//! (`mockk(relaxed = true)`, `runTest(timeout = …)`) was rejected as "unresolved function".
//!
//! The label, not the position, decides which parameter an argument is checked against. The named
//! mapping was computed, then discarded: the arguments were compacted into a dense list and matched
//! against the LEADING parameters, so the call resolved only when the supplied types happened to be
//! assignable at those positions — `f(a: Int = 1, b: Int = 2)` called as `f(b = 5)` "worked" while
//! `f(a: Int = 1, b: String = "z")` called as `f(b = "x")` did not. Selection
//! (`resolve_top_level_named_default_callable`) and the argument check now both use the parameter
//! slot the label names, and the `$default` mask marks exactly the unfilled slots.
//!
//! A VARARG slot is the exception: `$default` passes the array straight through and never fills it,
//! so an omitted vararg is an EMPTY array with its mask bit CLEAR. Masking it reached the callee as
//! `null` and tripped its non-null parameter check at runtime.
use super::common;

/// One kotlinc-built dependency covering the shapes: a leading default of a DIFFERENT type from the
/// named parameter (the case positional matching silently mis-checked), a vararg between defaults,
/// and a trailing defaulted lambda.
const LIB: &str = r#"
    package lib

    fun labelled(first: Int = 1, second: String = "z"): String = "$first/$second"

    fun spanning(
        name: String? = null,
        relaxed: Boolean = false,
        vararg more: Int,
        tail: Boolean = false,
        block: () -> Unit = {},
    ): String = "${name ?: "-"}/$relaxed/${more.size}/$tail"
"#;

/// Trailing-lambda builders whose lambda parameter is not a bare `() -> Unit`: a RECEIVER lambda, a
/// `suspend` receiver lambda, and a plain `suspend` lambda. The literal `{ }` must be typed from the
/// parameter its slot names before overload resolution sees it.
const LAMBDA_LIB: &str = r#"
    package lib

    import kotlin.coroutines.Continuation
    import kotlin.coroutines.CoroutineContext
    import kotlin.coroutines.EmptyCoroutineContext
    import kotlin.coroutines.startCoroutine

    class Scope(val budget: Int)

    private fun driver() = object : Continuation<Unit> {
        override val context: CoroutineContext get() = EmptyCoroutineContext
        override fun resumeWith(result: Result<Unit>) = result.getOrThrow()
    }

    fun withReceiver(label: String = "-", budget: Int = 7, body: Scope.() -> Unit): String {
        Scope(budget).body()
        return "$label/$budget"
    }

    fun withSuspendReceiver(label: String = "-", budget: Int = 7, body: suspend Scope.() -> Unit): String {
        body.startCoroutine(Scope(budget), driver())
        return "$label/$budget"
    }

    fun withSuspend(label: String = "-", budget: Int = 7, body: suspend () -> Unit): String {
        body.startCoroutine(driver())
        return "$label/$budget"
    }
"#;

#[test]
fn named_argument_skipping_a_default_resolves_and_runs() {
    let Some(libout) = common::compile_lib("named_skips_default", LIB) else {
        return;
    };
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let classpath = [libout, stdlib];
    // Each call names a parameter whose type differs from the omitted leading one, so a positional
    // check would compare the argument against the wrong declaration.
    let main = "import lib.labelled\n\
        import lib.spanning\n\
        fun box(): String {\n\
        \x20 if (labelled(second = \"x\") != \"1/x\") return \"labelled: ${labelled(second = \"x\")}\"\n\
        \x20 if (labelled(first = 5) != \"5/z\") return \"first: ${labelled(first = 5)}\"\n\
        \x20 if (spanning(relaxed = true) != \"-/true/0/false\") return \"relaxed: ${spanning(relaxed = true)}\"\n\
        \x20 if (spanning(tail = true) != \"-/false/0/true\") return \"tail: ${spanning(tail = true)}\"\n\
        \x20 if (spanning(name = \"n\", tail = true) != \"n/false/0/true\") return \"both: ${spanning(name = \"n\", tail = true)}\"\n\
        \x20 return \"OK\"\n\
        }\n";
    let Some(out) = common::compile_and_run_box(main, "Main", &classpath, Some(jdk.as_path()))
    else {
        panic!(
            "compile/run returned None: {:?}",
            common::front_end_diagnostics(main, &classpath, Some(jdk.as_path()))
        );
    };
    assert_eq!(out, "OK");
}

#[test]
fn trailing_lambda_is_typed_from_the_slot_a_named_argument_leaves_it() {
    // The literal `{ }` is typed BEFORE overload resolution, from the callee's block parameter — that
    // is what gives it its receiver and arity. The top-level shaping path mapped arguments
    // positionally, so `withReceiver(budget = 3) { }` aligned the `Int` against `label: String`, found
    // no applicable overload, and left the lambda as a bare `() -> Unit` — which then failed against
    // the erased `Function1`. Naming a parameter that ISN'T skipped over always worked, which is why
    // this looked like a receiver/suspend problem; all three lambda kinds fail identically.
    let Some(libout) = common::compile_lib("named_skips_default_lambda", LAMBDA_LIB) else {
        return;
    };
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let classpath = [libout, stdlib];
    // The `suspend` receiver body is EMPTY on purpose: a `suspend Receiver.() -> Unit` whose body
    // actually uses its receiver is a separate, pre-existing lowering gap ("this construct is not yet
    // supported by the IR backend") that the POSITIONAL form hits identically. Its return value still
    // proves what this test is about — `budget = 4` reached the parameter its label names, and the
    // literal was shaped as a suspend receiver lambda rather than a bare `() -> Unit`.
    let main = "import lib.withReceiver\n\
        import lib.withSuspend\n\
        import lib.withSuspendReceiver\n\
        fun box(): String {\n\
        \x20 var seen = 0\n\
        \x20 if (withReceiver(budget = 3) { seen += budget } != \"-/3\") return \"receiver\"\n\
        \x20 if (withSuspendReceiver(budget = 4) { } != \"-/4\") return \"suspend receiver\"\n\
        \x20 if (withSuspend(budget = 5) { seen += 5 } != \"-/5\") return \"suspend\"\n\
        \x20 if (withReceiver(label = \"x\") { seen += budget } != \"x/7\") return \"no gap\"\n\
        \x20 if (seen != 15) return \"bodies ran: $seen\"\n\
        \x20 return \"OK\"\n\
        }\n";
    let Some(out) = common::compile_and_run_box(main, "Main", &classpath, Some(jdk.as_path()))
    else {
        panic!(
            "compile/run returned None: {:?}",
            common::front_end_diagnostics(main, &classpath, Some(jdk.as_path()))
        );
    };
    assert_eq!(out, "OK");
}

#[test]
fn named_argument_type_is_still_checked_against_the_parameter_it_names() {
    // The mapping must not become permissive: a wrong type for the NAMED parameter is still an
    // error, and the message names THAT parameter's type, not the one at the argument's position.
    let Some(libout) = common::compile_lib("named_skips_default_bad", LIB) else {
        return;
    };
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let classpath = [libout, stdlib];
    let main = "import lib.labelled\n\
        fun box(): String = labelled(second = 5)\n";
    let diagnostics = common::front_end_diagnostics(main, &classpath, Some(jdk.as_path()));
    assert!(
        diagnostics.iter().any(|message| {
            message.contains("actual type is 'Int'") && message.contains("'String' was expected")
        }),
        "expected a String/Int mismatch on the named parameter, got {diagnostics:?}"
    );
}
