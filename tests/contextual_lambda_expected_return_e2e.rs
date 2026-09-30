//! A postponed lambda's body is inferred under the call's expected return type.
//!
//! `PerformanceCounter.getCallStack` is an expression-bodied function:
//! `getOrPut(threadLocal) { HashMap() }.getOrPut(...)`. `T` is fixed by the
//! `ThreadLocal<MutableMap<...>>` argument, so `{ HashMap() }` must be checked as
//! `() -> MutableMap<K, V>` while the enclosing signature is still being inferred.
//! Typing the constructor with no expectation defaulted its type arguments and dropped
//! the module's signatures.

use super::common;

#[test]
fn thread_local_lambda_constructs_the_expected_map() {
    let src = "import java.lang.ThreadLocal\n\
        fun <T> getOrPut(threadLocal: ThreadLocal<T>, default: () -> T): T = default()\n\
        fun load(local: ThreadLocal<MutableMap<Int, String>>) = getOrPut(local) { HashMap() }\n\
        fun box(): String {\n\
        \x20   val local = ThreadLocal<MutableMap<Int, String>>()\n\
        \x20   val map = load(local)\n\
        \x20   map[1] = \"OK\"\n\
        \x20   return map.getOrPut(1) { \"NO\" }\n\
        }\n";
    common::assert_accepted_like_kotlinc(src);
    assert_eq!(common::kotlinc_box_result(src), "OK");
    common::expect_box_ok_with_stdlib(src, "ThreadLocalExpectedMap");
}

#[test]
fn return_only_call_in_a_lambda_uses_the_expected_type() {
    let src = "fun <T> pick(value: T, default: () -> T): T = default()\n\
        fun load(seed: MutableMap<Int, String>) = pick(seed) { mutableMapOf() }\n\
        fun box(): String {\n\
        \x20   val seed: MutableMap<Int, String> = HashMap()\n\
        \x20   val map = load(seed)\n\
        \x20   map[1] = \"OK\"\n\
        \x20   return map[1]!!\n\
        }\n";
    common::assert_accepted_like_kotlinc(src);
    assert_eq!(common::kotlinc_box_result(src), "OK");
    common::expect_box_ok_with_stdlib(src, "ExpectedMutableMapOf");
}

#[test]
fn a_statement_before_the_lambda_result_does_not_fail_signature_inference() {
    let src = "import kotlin.coroutines.*\n\
        import kotlin.coroutines.intrinsics.*\n\
        suspend fun suspendHere() = suspendCoroutineUninterceptedOrReturn {\n\
        \x20   it.resume(Unit)\n\
        \x20   COROUTINE_SUSPENDED\n\
        }\n\
        fun box(): String = \"OK\"\n";
    common::assert_accepted_like_kotlinc(src);
}

#[test]
fn conditional_constructor_in_a_lambda_uses_the_expected_type() {
    let src = "fun <T> pick(value: T, default: () -> T): T = default()\n\
        fun load(seed: ArrayList<String>) =\n\
        \x20   pick(seed) { if (seed.isEmpty()) ArrayList() else ArrayList() }\n\
        fun box(): String {\n\
        \x20   val seed = ArrayList<String>()\n\
        \x20   val list = load(seed)\n\
        \x20   list.add(\"OK\")\n\
        \x20   return list[0]\n\
        }\n";
    common::assert_accepted_like_kotlinc(src);
    assert_eq!(common::kotlinc_box_result(src), "OK");
    common::expect_box_ok_with_stdlib(src, "ExpectedArrayListBranch");
}
