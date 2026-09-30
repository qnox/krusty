//! A postponed lambda's body is inferred under the call's expected return type.
//!
//! `PerformanceCounter.getCallStack` is `getOrPut(threadLocal) { HashMap() }.getOrPut(...)`.
//! `T` is fixed by the `ThreadLocal<MutableMap<...>>` argument, so `{ HashMap() }` must be
//! checked as `() -> MutableMap<K, V>`. Typing the constructor with no expectation defaulted
//! its type arguments and dropped the whole module's signatures.

use super::common;

#[test]
fn thread_local_lambda_constructs_the_expected_map() {
    let src = "import java.lang.ThreadLocal\n\
        fun <T> getOrPut(threadLocal: ThreadLocal<T>, default: () -> T): T = default()\n\
        fun box(): String {\n\
        \x20   val local = ThreadLocal<MutableMap<Int, String>>()\n\
        \x20   val map = getOrPut(local) { HashMap() }\n\
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
        fun box(): String {\n\
        \x20   val seed: MutableMap<Int, String> = HashMap()\n\
        \x20   val map = pick(seed) { mutableMapOf() }\n\
        \x20   map[1] = \"OK\"\n\
        \x20   return map[1]!!\n\
        }\n";
    common::assert_accepted_like_kotlinc(src);
    assert_eq!(common::kotlinc_box_result(src), "OK");
    common::expect_box_ok_with_stdlib(src, "ExpectedMutableMapOf");
}

#[test]
fn conditional_constructor_in_a_lambda_uses_the_expected_type() {
    let src = "fun <T> pick(value: T, default: () -> T): T = default()\n\
        fun box(): String {\n\
        \x20   val seed = ArrayList<String>()\n\
        \x20   val list = pick(seed) { if (seed.isEmpty()) ArrayList() else ArrayList() }\n\
        \x20   list.add(\"OK\")\n\
        \x20   return list[0]\n\
        }\n";
    common::assert_accepted_like_kotlinc(src);
    assert_eq!(common::kotlinc_box_result(src), "OK");
    common::expect_box_ok_with_stdlib(src, "ExpectedArrayListBranch");
}
