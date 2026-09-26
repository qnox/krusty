//! Suspend lambdas compiled to classes of their own, as kotlinc's `SuspendLambdaLowering` does
//! (`docs/JVM_INLINE_BEFORE_CPS.md` §1a, step 6).
//!
//! The lambda extends `SuspendLambda`, implements its `FunctionN` and is its own continuation. Its
//! body is `invokeSuspend`, whose state machine kotlinc's coroutine transformer builds in its
//! lambda mode when the class is written. The captured values are final `$name` fields, the
//! parameters the body reads are spill-named fields that `create` or the typed `invoke` fill on a
//! fresh copy, and the erased `invoke` bridges to the typed one. Each member's instructions and
//! local-variable table match kotlinc's.

use super::common;

/// The members kotlinc writes for a suspend lambda, as `javap` prints each one's declaration.
fn members(class: &str, create: bool) -> Vec<String> {
    let simple = class.rsplit('/').next().unwrap_or(class);
    let mut members = vec![
        format!("{simple}("),
        "public final java.lang.Object invokeSuspend(".to_string(),
    ];
    if create {
        members
            .push("public final kotlin.coroutines.Continuation<kotlin.Unit> create(".to_string());
    }
    members.push("public final java.lang.Object invoke(".to_string());
    members.push("public java.lang.Object invoke(".to_string());
    members
}

/// Compare each of `methods` of `class` with kotlinc's.
fn expect_methods_match(name: &str, src: &str, class: &str, methods: &[String]) {
    for method in methods {
        match common::method_code_diff_against_kotlinc(name, &[], src, class, method) {
            None => panic!("reference kotlinc is provisioned"),
            Some(Ok(())) => {}
            Some(Err(difference)) => panic!("{difference}"),
        }
    }
}

#[test]
fn a_one_parameter_lambda_matches_kotlinc() {
    // `s` is read back from `L$0` at the top of `invokeSuspend`; `create(Object, Continuation)`
    // stores it on a fresh copy, which the typed `invoke` starts.
    let src = "suspend fun leaf(): Int = 1\n\
fun take(s: String) {}\n\
fun make(): suspend (String) -> Int = { s: String -> take(s); leaf() + s.length }\n";
    let class = "SlOneKt$make$1";
    expect_methods_match("SlOne", src, class, &members(class, true));
}

#[test]
fn a_parameterless_lambda_creates_itself_from_its_completion_alone() {
    let src = "suspend fun leaf(): Int = 1\n\
fun make(): suspend () -> Unit = { leaf() }\n";
    let class = "SlNoneKt$make$1";
    expect_methods_match("SlNone", src, class, &members(class, true));
}

#[test]
fn a_receiver_lambda_with_captures_fills_its_copy_in_invoke() {
    // Two parameters: no `create`, so `invoke` builds the copy from the captured `$x` and `$n` and
    // stores the receiver (`I$0`, private) and `s` (`L$0`) itself.
    let src = "suspend fun leaf(): Int = 1\n\
fun make(x: String, n: Long): suspend Int.(String) -> Long =\n\
    { s -> leaf() + x.length + s.length + this + n }\n";
    let class = "SlReceiverKt$make$1";
    expect_methods_match("SlReceiver", src, class, &members(class, false));
}

#[test]
fn a_primitive_parameter_is_unboxed_into_its_field() {
    let src = "suspend fun leaf(): Int = 1\n\
fun make(): suspend (Int) -> Int = { n -> leaf() + n }\n";
    let class = "SlPrimitiveKt$make$1";
    expect_methods_match("SlPrimitive", src, class, &members(class, true));
}

#[test]
fn a_local_live_across_a_suspension_is_spilled_to_a_field_the_transformer_adds() {
    // `a` is spilled to `L$1`, declared ahead of `label` and the parameter's `L$0`. The members
    // that fill the copy match kotlinc's. `invokeSuspend`'s instructions do too, but not yet its
    // local-variable table: a lambda's body lowers as `return <block>`, so `a`'s range closes with
    // that block, one instruction before kotlinc's, which ends it after the `areturn`. Plain
    // lambdas share the gap.
    let src = "suspend fun leaf(): Int = 1\n\
fun make(x: String): suspend (String) -> String = { s -> val a = s + x; leaf(); a + s }\n";
    let class = "SlSpillKt$make$1";
    let filled = members(class, true)
        .into_iter()
        .filter(|member| !member.contains("invokeSuspend"))
        .collect::<Vec<_>>();
    expect_methods_match("SlSpill", src, class, &filled);
}

#[test]
fn a_member_s_lambda_captures_its_receiver_as_this_0() {
    // The constructor names the captured receiver `$receiver`; its field is `this$0`.
    let src = "suspend fun leaf(): Int = 1\n\
fun keep(a: Any) {}\n\
class Box(val v: Int) {\n\
    fun f() { keep(suspend { leaf() + v }) }\n\
}\n";
    let class = "Box$f$1";
    expect_methods_match("SlMember", src, class, &members(class, true));
}

#[test]
fn a_captured_var_is_shared_through_its_ref_cell() {
    let src = "suspend fun leaf(): Int = 1\n\
fun keep(a: Any) {}\n\
fun count() { var c = 1; keep(suspend { c += leaf() }) }\n";
    let class = "SlSharedKt$count$1";
    expect_methods_match("SlShared", src, class, &members(class, true));
}

#[test]
fn the_lambda_value_is_cast_only_where_its_function_type_is_needed() {
    // `make` returns the value as its `Function2` and casts it; `pass` hands it on as `Any`.
    let src = "suspend fun leaf(): Int = 1\n\
fun keep(a: Any) {}\n\
fun make(): suspend (String) -> Int = { s: String -> leaf() + s.length }\n\
fun pass() { keep(suspend { leaf() }) }\n";
    let methods = [
        "public static final kotlin.jvm.functions.Function2<".to_string(),
        "public static final void pass(".to_string(),
    ];
    expect_methods_match("SlSite", src, "SlSiteKt", &methods);
}

#[test]
fn a_lambda_class_runs_through_create_invoke_and_its_bridge() {
    // The erased bridge, the typed `invoke` and `create` each start a fresh copy of the lambda
    // with the caller's continuation, which receives the result.
    let src = "import kotlin.coroutines.*\n\
class Sink : Continuation<Any?> {\n\
    var value: Any? = null\n\
    override val context: CoroutineContext = EmptyCoroutineContext\n\
    override fun resumeWith(result: Result<Any?>) {}\n\
}\n\
suspend fun leaf(): Int = 1\n\
fun one(x: String): suspend (String) -> String = { s -> val a = s + x; leaf(); a + s }\n\
fun two(n: Long): suspend Int.(String) -> Long = { s -> leaf() + s.length + this + n }\n\
fun box(): String {\n\
    val sink = Sink()\n\
    val first = (one(\"!\") as Function2<String, Continuation<Any?>, Any?>)(\"hi\", sink)\n\
    val second = (two(5L) as Function3<Int, String, Continuation<Any?>, Any?>)(7, \"q\", sink)\n\
    return if (first == \"hi!hi\" && second == 14L) \"OK\" else \"F:$first:$second\"\n\
}\n";
    common::expect_box_ok_with_stdlib(src, "SlRun");
}
