//! Calls to member suspend functions as suspension points of a body that kotlinc's coroutine
//! transformer builds the machine of (`docs/JVM_INLINE_BEFORE_CPS.md` §1a).
//!
//! A call to a member of a class in the same file, on an explicit receiver, on `this`, or on a
//! suspend lambda's extension receiver, is a plain call like a call to a top-level function. The
//! emitter marks it as kotlinc's codegen does, so the named function or the lambda class goes
//! through the transformer instead of keeping the IR machine.

use super::common;

const NAMED: &str = "class Api(val base: Int) {\n\
    suspend fun get(n: Int): Int = n + base\n\
    suspend fun twice(n: Int): Int {\n\
        val a = get(n)\n\
        return a + get(a)\n\
    }\n\
}\n\
interface Source {\n\
    suspend fun next(): Int\n\
}\n\
suspend fun use(api: Api, n: Int): Int {\n\
    val v = api.get(n)\n\
    return v + n\n\
}\n\
suspend fun drain(source: Source): Int {\n\
    val first = source.next()\n\
    return first + source.next()\n\
}\n";

fn expect_method_matches(name: &str, src: &str, class: &str, method: &str) {
    match common::method_code_diff_against_kotlinc(name, &[], src, class, method) {
        None => panic!("reference kotlinc is provisioned"),
        Some(Ok(())) => {}
        Some(Err(difference)) => panic!("{difference}"),
    }
}

#[test]
fn a_function_calling_members_keeps_kotlinc_s_continuation_class() {
    for class in [
        "MemberPointsKt$use$1",
        "MemberPointsKt$drain$1",
        "Api$twice$1",
    ] {
        let result = common::byte_diff_against_kotlinc_cp(
            "MemberPoints",
            NAMED,
            class,
            &[common::stdlib_jar()],
        )
        .expect("reference kotlinc is provisioned");
        result.expect("the continuation class is byte-identical to kotlinc's");
    }
}

#[test]
fn a_function_calling_members_matches_kotlinc_s_instructions() {
    for (class, method) in [
        (
            "MemberPointsKt",
            "public static final java.lang.Object use(",
        ),
        (
            "MemberPointsKt",
            "public static final java.lang.Object drain(",
        ),
        ("Api", "public final java.lang.Object twice("),
    ] {
        expect_method_matches("MemberPoints", NAMED, class, method);
    }
}

#[test]
fn a_receiver_lambda_calling_its_receiver_s_member_matches_kotlinc() {
    // The call's receiver is the lambda's own extension receiver, kept in the private `L$0`. The
    // lambda is not a call argument, so kotlinc names that receiver `<this>`; as an argument
    // (`builder { step(1) }`) kotlinc names it after the call, `$this$builder`, which krusty does
    // not yet.
    let src = "class Controller {\n\
    suspend fun step(n: Int): String = \"s\" + n\n\
}\n\
var sink = \"\"\n\
fun start(): suspend Controller.() -> Unit = {\n\
    sink = step(1)\n\
    sink = step(2)\n\
}\n";
    let class = "MemberLambdaKt$start$1";
    for method in [
        "MemberLambdaKt$start$1(",
        "public final java.lang.Object invokeSuspend(",
        "public final kotlin.coroutines.Continuation<kotlin.Unit> create(",
        "public final java.lang.Object invoke(",
        "public java.lang.Object invoke(",
    ] {
        expect_method_matches("MemberLambda", src, class, method);
    }
}

#[test]
fn member_calls_that_suspend_resume_where_they_left_off() {
    // Each member call really suspends, so the named function and the lambda each resume twice
    // through their machines.
    let src = "import kotlin.coroutines.*\n\
class Done : Continuation<Unit> {\n\
  override val context: CoroutineContext = EmptyCoroutineContext\n\
  override fun resumeWith(result: Result<Unit>) { result.getOrThrow() }\n\
}\n\
var parked: Continuation<Unit>? = null\n\
class Api(val tag: String) {\n\
    suspend fun get(n: Int): String {\n\
        suspendCoroutine<Unit> { parked = it }\n\
        return tag + n\n\
    }\n\
}\n\
suspend fun use(api: Api): String {\n\
    val a = api.get(1)\n\
    return a + api.get(2)\n\
}\n\
fun builder(api: Api, c: suspend Api.() -> Unit) { c.startCoroutine(api, Done()) }\n\
fun box(): String {\n\
    var r = \"none\"\n\
    var s = \"none\"\n\
    val api = Api(\"a\")\n\
    suspend { r = use(api) }.startCoroutine(Done())\n\
    parked!!.resume(Unit)\n\
    parked!!.resume(Unit)\n\
    builder(api) { s = get(3) + get(4) }\n\
    parked!!.resume(Unit)\n\
    parked!!.resume(Unit)\n\
    return if (r == \"a1a2\" && s == \"a3a4\") \"OK\" else \"F:$r:$s\"\n\
}\n";
    common::expect_box_ok_with_stdlib(src, "MemberPointsResume");
}

// Promoted from `backend_rejection_coverage_e2e`: the IR machine declined two safe-call
// suspensions in one expression, and the transformer takes them. The instructions still differ
// from kotlinc's around the second `?.`/`?:`: kotlinc stores the first operand after the null
// check, krusty before it with a `dup`, as for any safe call with a pending operand.
const SAFE_CALLS: &str = "class Box(val v: Int) { suspend fun d(): Int = v }\n\
suspend fun f(b: Box?): Int { return (b?.d() ?: 0) + (b?.d() ?: 0) }\n";

#[test]
fn two_safe_member_calls_in_one_expression_run() {
    let src = format!(
        "import kotlin.coroutines.*\n\
{SAFE_CALLS}\
class Done : Continuation<Unit> {{\n\
  override val context: CoroutineContext = EmptyCoroutineContext\n\
  override fun resumeWith(result: Result<Unit>) {{ result.getOrThrow() }}\n\
}}\n\
fun box(): String {{\n\
    var some = -1\n\
    var none = -1\n\
    suspend {{ some = f(Box(3)); none = f(null) }}.startCoroutine(Done())\n\
    return if (some == 6 && none == 0) \"OK\" else \"F:$some:$none\"\n\
}}\n"
    );
    common::expect_box_ok_with_stdlib(&src, "SafeCallsRun");
}
