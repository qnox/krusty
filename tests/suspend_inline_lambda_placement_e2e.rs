//! A call to a dependency's inline suspend function is spliced from its `$$forInline` copy. When the
//! literal lambda it takes suspends, kotlinc's coroutine transformer builds one machine over the
//! caller, the copy and the placed lambda: the copy's marked call and the lambda's suspensions are
//! the machine's points, the inline call itself is none, and the copy's continuation parameter
//! reads the caller's continuation. Each point really suspends here, so every run resumes through
//! each state.

use super::common;

const LIB: &str = "package lib\n\
import kotlin.coroutines.*\n\
import kotlin.coroutines.intrinsics.*\n\
var pending: Continuation<String>? = null\n\
class Builder { var token = \"\" }\n\
class Client {\n\
    \x20   suspend fun exec(b: Builder): String =\n\
    \x20       suspendCoroutineUninterceptedOrReturn<String> { c -> pending = c; COROUTINE_SUSPENDED } + b.token\n\
}\n\
suspend inline fun Client.get(block: Builder.() -> Unit): String = exec(Builder().apply(block))\n";

const MAIN: &str = "import lib.*\n\
import kotlin.coroutines.*\n\
import kotlin.coroutines.intrinsics.*\n\
var saved: Continuation<String>? = null\n\
suspend fun tok(): String = suspendCoroutineUninterceptedOrReturn { c -> saved = c; COROUTINE_SUSPENDED }\n\
class Api(val c: Client) {\n\
    \x20   suspend fun list(id: String): String {\n\
    \x20       val r = c.get { token = tok() + id }\n\
    \x20       return r\n\
    \x20   }\n\
}\n\
suspend fun twice(c: Client, n: Int): String {\n\
    \x20   var acc = \"\"\n\
    \x20   for (i in 0 until n) {\n\
    \x20       acc += c.get { token = tok() + i }\n\
    \x20   }\n\
    \x20   return acc\n\
}\n\
suspend fun early(c: Client, id: String): String {\n\
    \x20   val r = c.get {\n\
    \x20       if (id.isEmpty()) return \"early\"\n\
    \x20       token = tok() + id\n\
    \x20   }\n\
    \x20   return r\n\
}\n\
fun drive(block: suspend () -> String): String {\n\
    \x20   var r = \"none\"\n\
    \x20   block.startCoroutine(Continuation(EmptyCoroutineContext) { r = it.getOrThrow() })\n\
    \x20   while (r == \"none\") {\n\
    \x20       val s = saved\n\
    \x20       val p = pending\n\
    \x20       if (s != null) { saved = null; s.resume(\"t\") }\n\
    \x20       else if (p != null) { pending = null; p.resume(\"p\") }\n\
    \x20       else return \"stuck\"\n\
    \x20   }\n\
    \x20   return r\n\
}\n\
fun box(): String {\n\
    \x20   val c = Client()\n\
    \x20   val listed = drive { Api(c).list(\"x\") }\n\
    \x20   if (listed != \"ptx\") return \"list: $listed\"\n\
    \x20   val lambda = drive { c.get { token = tok() + \"L\" } }\n\
    \x20   if (lambda != \"ptL\") return \"suspend lambda: $lambda\"\n\
    \x20   val looped = drive { twice(c, 2) }\n\
    \x20   if (looped != \"pt0pt1\") return \"loop: $looped\"\n\
    \x20   val left = drive { early(c, \"\") }\n\
    \x20   if (left != \"early\") return \"non-local return: $left\"\n\
    \x20   val kept = drive { early(c, \"e\") }\n\
    \x20   if (kept != \"pte\") return \"no return: $kept\"\n\
    \x20   return \"OK\"\n\
}\n";

/// A named member, a suspend lambda, a loop whose accumulator stays on the stack across the call,
/// and a lambda that may leave by a non-local `return`: each resumes through every state.
#[test]
fn a_suspending_lambda_of_an_inline_suspend_dependency_call_resumes() {
    assert_eq!(
        common::expect_box_run_against("inline_suspend_lambda_placement", LIB, MAIN).as_deref(),
        Some("OK")
    );
}

/// The member and its continuation are the reference compiler's, byte for byte.
#[test]
fn a_suspending_lambda_of_an_inline_suspend_dependency_call_matches_kotlinc() {
    let library = common::compile_lib("inline_suspend_lambda_placement", LIB)
        .expect("krusty builds the library");
    let classpath = [library, common::stdlib_jar()];
    for class in ["Api", "Api$list$1"] {
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
