//! A `suspend R.() -> Unit` lambda passed to a CLASSPATH (separately-compiled) function. The
//! same-module shape already lowers (`suspend_receiver_lambda_e2e`), and so does a non-suspending
//! classpath receiver lambda (`classpath_receiver_lambda_e2e`); the combination used to skip the
//! whole file with "krusty: this construct is not yet supported by the IR backend" as soon as the
//! body actually touched the receiver. Round-tripped on a real JVM against a kotlinc-built jar.

use super::common;

/// The dependency: a builder that starts the coroutine with the receiver overload of
/// `startCoroutine`, so the lambda is a real `suspend Scope.() -> Unit` in `@Metadata`.
const LIB: &str = r#"
package lib

import kotlin.coroutines.Continuation
import kotlin.coroutines.CoroutineContext
import kotlin.coroutines.EmptyCoroutineContext
import kotlin.coroutines.startCoroutine

class Scope(val budget: Int)

fun withScope(label: String = "-", budget: Int = 7, body: suspend Scope.() -> Unit): String {
    body.startCoroutine(Scope(budget), object : Continuation<Unit> {
        override val context: CoroutineContext get() = EmptyCoroutineContext
        override fun resumeWith(result: Result<Unit>) = result.getOrThrow()
    })
    return "$label/$budget"
}
"#;

/// An empty body needs no receiver, and always lowered.
#[test]
fn classpath_suspend_receiver_lambda_empty_body() {
    common::expect_box_ok_against(
        "csrl-empty",
        LIB,
        "import lib.withScope\n\
         fun box(): String {\n\
         \x20 val r = withScope { }\n\
         \x20 return if (r == \"-/7\") \"OK\" else \"FAIL $r\"\n\
         }\n",
    );
}

/// Non-capturing body that READS the receiver's property through the implicit `this`.
#[test]
fn classpath_suspend_receiver_lambda_reads_receiver() {
    common::expect_box_ok_against(
        "csrl-read",
        LIB,
        "import lib.withScope\n\
         fun box(): String {\n\
         \x20 val r = withScope { check(budget == 7) }\n\
         \x20 return if (r == \"-/7\") \"OK\" else \"FAIL $r\"\n\
         }\n",
    );
}

/// Capturing body: the lambda closes over a local AND reads the receiver.
#[test]
fn classpath_suspend_receiver_lambda_captures() {
    common::expect_box_ok_against(
        "csrl-capture",
        LIB,
        "import lib.withScope\n\
         fun box(): String {\n\
         \x20 var seen = 0\n\
         \x20 withScope { seen += budget }\n\
         \x20 return if (seen == 7) \"OK\" else \"FAIL $seen\"\n\
         }\n",
    );
}

/// A named argument ahead of the trailing lambda, with one default omitted.
#[test]
fn classpath_suspend_receiver_lambda_named_arg() {
    common::expect_box_ok_against(
        "csrl-named",
        LIB,
        "import lib.withScope\n\
         fun box(): String {\n\
         \x20 val r = withScope(budget = 3) { check(budget == 3) }\n\
         \x20 return if (r == \"-/3\") \"OK\" else \"FAIL $r\"\n\
         }\n",
    );
}
