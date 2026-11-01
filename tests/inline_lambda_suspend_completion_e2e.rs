//! The continuation an inline suspend dependency call receives inside an inline lambda's body.
//!
//! kotlinc compiles an inline lambda's body as a method of its own, which reads the enclosing
//! function's continuation as a value the inliner binds rather than as a local of the caller. The
//! inlined callee's continuation parameter therefore gets a local of its own, `$completion$iv`,
//! which is spilled at the callee's suspension like any other live local. Called directly from the
//! function's body, the callee reads the caller's continuation local and gets no such local.
//! Both shapes are written below, for a plain inline lambda and for a suspend one.

use super::common;

const LIB: &str = "package lib\n\
import kotlin.coroutines.*\n\
import kotlin.coroutines.intrinsics.*\n\
var pending: Continuation<String>? = null\n\
suspend fun fetch(s: String): String =\n\
    \x20   suspendCoroutineUninterceptedOrReturn { c -> pending = c; COROUTINE_SUSPENDED }\n\
suspend inline fun String.load(path: String): String {\n\
    \x20   val got = fetch(this + path)\n\
    \x20   return got + path\n\
}\n\
suspend inline fun around(block: suspend () -> String): String = block()\n";

const MAIN: &str = "import lib.*\n\
import kotlin.coroutines.*\n\
class Api(val base: String) {\n\
    \x20   suspend fun direct(path: String): String {\n\
    \x20       val trimmed = base.trim()\n\
    \x20       val got = trimmed.load(path)\n\
    \x20       return got\n\
    \x20   }\n\
    \x20   suspend fun inLambda(path: String): String {\n\
    \x20       val trimmed = base.trim()\n\
    \x20       return trimmed.let { value -> value.load(path) }\n\
    \x20   }\n\
}\n\
class Placed(val base: String) {\n\
    \x20   suspend fun inSuspendLambda(path: String): String {\n\
    \x20       val trimmed = base.trim()\n\
    \x20       return around { trimmed.load(path) }\n\
    \x20   }\n\
}\n\
fun drive(block: suspend () -> String): String {\n\
    \x20   var r = \"none\"\n\
    \x20   block.startCoroutine(Continuation(EmptyCoroutineContext) { r = it.getOrThrow() })\n\
    \x20   while (r == \"none\") {\n\
    \x20       val p = pending ?: return \"stuck\"\n\
    \x20       pending = null\n\
    \x20       p.resume(\"<\")\n\
    \x20   }\n\
    \x20   return r\n\
}\n\
fun box(): String {\n\
    \x20   val api = Api(\" a \")\n\
    \x20   val direct = drive { api.direct(\"/d\") }\n\
    \x20   if (direct != \"</d\") return \"direct: $direct\"\n\
    \x20   val lambda = drive { api.inLambda(\"/l\") }\n\
    \x20   if (lambda != \"</l\") return \"lambda: $lambda\"\n\
    \x20   val suspending = drive { Placed(\" a \").inSuspendLambda(\"/s\") }\n\
    \x20   if (suspending != \"</s\") return \"suspend lambda: $suspending\"\n\
    \x20   return \"OK\"\n\
}\n";

/// Each call resumes through its suspension with the callee's continuation in its own local.
#[test]
fn an_inline_suspend_call_in_an_inline_lambda_resumes() {
    assert_eq!(
        common::expect_box_run_against("inline_lambda_suspend_completion", LIB, MAIN).as_deref(),
        Some("OK")
    );
}

/// The functions and their continuations, spill fields included, are the reference compiler's.
#[test]
fn an_inline_suspend_call_in_an_inline_lambda_matches_kotlinc() {
    let library = common::compile_lib("inline_lambda_suspend_completion", LIB)
        .expect("krusty builds the library");
    let classpath = [library, common::stdlib_jar()];
    // `Placed` itself is compared through its continuation only: the method inliner does not
    // yet end a placed suspend lambda's expansion with the `nop` kotlinc writes there.
    for class in [
        "Api",
        "Api$direct$1",
        "Api$inLambda$1",
        "Placed$inSuspendLambda$1",
    ] {
        let compared =
            common::compare_with_kotlinc_plugin("Main", MAIN, class, &classpath, "25", &[])
                .expect("reference kotlinc and javap must be available");
        assert!(
            compared.krusty_bytes == compared.reference_bytes,
            "{class} differs\n--- kotlinc ---\n{}\n--- krusty ---\n{}",
            compared.reference,
            compared.krusty
        );
    }
}
