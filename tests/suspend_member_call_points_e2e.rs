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
    // (`builder { step(1) }`) kotlinc names it after the call, `$this$builder`
    // (`lambda_receiver_label_e2e`).
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

// A defaulted member call goes through the member's `$default` stub. Only the stub's invoke is the
// suspension point: the placeholders and the mask group before it mark the call's line and no
// suspension marker.
const DEFAULTED: &str = "class Api(val base: Int) {\n\
    suspend fun get(n: Int = 1): Int = n + base\n\
}\n\
suspend fun useDefault(api: Api): Int {\n\
    val a = api.get()\n\
    return a + api.get(2)\n\
}\n";

#[test]
fn a_defaulted_member_call_matches_kotlinc() {
    expect_method_matches(
        "DefaultedMember",
        DEFAULTED,
        "DefaultedMemberKt",
        "public static final java.lang.Object useDefault(",
    );
    common::byte_diff_against_kotlinc_cp(
        "DefaultedMember",
        DEFAULTED,
        "DefaultedMemberKt$useDefault$1",
        &[common::stdlib_jar()],
    )
    .expect("reference kotlinc is provisioned")
    .expect("the continuation class is byte-identical to kotlinc's");
}

#[test]
fn a_defaulted_member_call_that_suspends_resumes_once() {
    let src = "import kotlin.coroutines.*\n\
class Done : Continuation<Unit> {\n\
  override val context: CoroutineContext = EmptyCoroutineContext\n\
  override fun resumeWith(result: Result<Unit>) { result.getOrThrow() }\n\
}\n\
var parked: Continuation<Unit>? = null\n\
var calls = 0\n\
class Api(val tag: String) {\n\
    suspend fun get(n: Int = 1): String {\n\
        calls++\n\
        suspendCoroutine<Unit> { parked = it }\n\
        return tag + n\n\
    }\n\
}\n\
suspend fun useDefault(api: Api): String {\n\
    val a = api.get()\n\
    return a + api.get(2)\n\
}\n\
fun box(): String {\n\
    var r = \"none\"\n\
    suspend { r = useDefault(Api(\"a\")) }.startCoroutine(Done())\n\
    parked!!.resume(Unit)\n\
    parked!!.resume(Unit)\n\
    return if (r == \"a1a2\" && calls == 2) \"OK\" else \"F:$r:$calls\"\n\
}\n";
    common::expect_box_ok_with_stdlib(src, "DefaultedMemberResume");
}

// A member extension called inside its class: `this` is the dispatch receiver and the written
// receiver the extension receiver. Each receiver and argument is evaluated once, left to right,
// though the call suspends between them.
const MEMBER_EXTENSION: &str = "import kotlin.coroutines.*\n\
class Done : Continuation<Unit> {\n\
  override val context: CoroutineContext = EmptyCoroutineContext\n\
  override fun resumeWith(result: Result<Unit>) { result.getOrThrow() }\n\
}\n\
var parked: Continuation<Unit>? = null\n\
class Recorder {\n\
    var log = \"\"\n\
    fun <T> note(tag: String, value: T): T {\n\
        log += tag\n\
        return value\n\
    }\n\
}\n\
class Api(val tag: String)\n\
class Scope(val name: String) {\n\
    suspend fun Api.fetch(n: Int): String {\n\
        suspendCoroutine<Unit> { parked = it }\n\
        return name + tag + n\n\
    }\n\
    suspend fun go(r: Recorder, api: Api): String =\n\
        r.note(\"e\", api).fetch(r.note(\"v\", 1)) + r.note(\"w\", api).fetch(2)\n\
}\n\
fun box(): String {\n\
    val r = Recorder()\n\
    var s = \"none\"\n\
    suspend { s = Scope(\"s\").go(r, Api(\"a\")) }.startCoroutine(Done())\n\
    val first = r.log\n\
    parked!!.resume(Unit)\n\
    val second = r.log\n\
    parked!!.resume(Unit)\n\
    val ok = first == \"ev\" && second == \"evw\" && r.log == \"evw\" && s == \"sa1sa2\"\n\
    return if (ok) \"OK\" else \"F:$first:$second:${r.log}:$s\"\n\
}\n";

#[test]
fn a_member_extension_call_evaluates_its_receivers_once_across_suspension() {
    common::expect_box_ok_with_stdlib(MEMBER_EXTENSION, "MemberExtensionResume");
}

#[test]
fn a_member_extension_call_across_suspension_matches_kotlinc() {
    let src = "class Recorder {\n\
    fun <T> note(value: T): T = value\n\
}\n\
class Api(val tag: String)\n\
class Scope(val name: String) {\n\
    suspend fun pause() {}\n\
    suspend fun Api.fetch(n: Int): String {\n\
        pause()\n\
        return name\n\
    }\n\
    suspend fun go(r: Recorder, api: Api): String = r.note(api).fetch(r.note(1))\n\
}\n";
    expect_method_matches(
        "MemberExtension",
        src,
        "Scope",
        "public final java.lang.Object go(",
    );
}
